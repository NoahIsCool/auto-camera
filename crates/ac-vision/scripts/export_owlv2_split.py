"""Exports OWLv2 as two ONNX graphs so ac-vision can cache text embeddings.

    text_encoder.onnx  input_ids [Q, 16] i64, attention_mask [Q, 16] i64
                       -> text_embeds [Q, 512] f32 (L2-normalized)
    detector.onnx      pixel_values [1, 3, 960, 960] f32, query_embeds [1, Q, 512] f32
                       -> logits [1, 3600, Q] f32, pred_boxes [1, 3600, 4] f32 (cx, cy, w, h)
    tokenizer.json     the model's CLIP tokenizer

STATUS: UNVALIDATED. Written against the Hugging Face transformers
Owlv2ForObjectDetection implementation (image_embedder, class_predictor,
box_predictor); it has not been run yet. Check the parity printout at the end
before trusting the export.

Usage:
    pip install torch transformers onnx onnxruntime
    python export_owlv2_split.py [model-id] [output-dir]
"""

import os
import sys

import numpy as np
import torch
from transformers import Owlv2ForObjectDetection, Owlv2Processor

MODEL = sys.argv[1] if len(sys.argv) > 1 else "google/owlv2-base-patch16-ensemble"
OUT = sys.argv[2] if len(sys.argv) > 2 else "owlv2-split-onnx"
os.makedirs(OUT, exist_ok=True)

model = Owlv2ForObjectDetection.from_pretrained(MODEL).eval()
processor = Owlv2Processor.from_pretrained(MODEL)
processor.tokenizer.save_pretrained(OUT)
side = processor.image_processor.size["height"]
dim = model.config.projection_dim


class TextEncoder(torch.nn.Module):
    """Prompt tokens -> normalized query embeddings, as Owlv2Model computes them."""

    def __init__(self, m):
        super().__init__()
        self.m = m

    def forward(self, input_ids, attention_mask):
        pooled = self.m.owlv2.text_model(input_ids=input_ids, attention_mask=attention_mask)[1]
        emb = self.m.owlv2.text_projection(pooled)
        return emb / torch.linalg.norm(emb, ord=2, dim=-1, keepdim=True)


class Detector(torch.nn.Module):
    """Image + query embeddings -> per-patch logits and boxes."""

    def __init__(self, m):
        super().__init__()
        self.m = m

    def forward(self, pixel_values, query_embeds):
        feature_map = self.m.image_embedder(pixel_values=pixel_values)[0]
        b, h, w, d = feature_map.shape
        image_feats = torch.reshape(feature_map, (b, h * w, d))
        logits, _ = self.m.class_predictor(image_feats, query_embeds, None)
        boxes = self.m.box_predictor(image_feats, feature_map)
        return logits, boxes


prompts = ["a person", "guitar"]
tok = processor.tokenizer(prompts, padding="max_length", max_length=16, return_tensors="pt")
pixels = torch.rand(1, 3, side, side)

with torch.no_grad():
    torch.onnx.export(
        TextEncoder(model),
        (tok.input_ids, tok.attention_mask),
        os.path.join(OUT, "text_encoder.onnx"),
        input_names=["input_ids", "attention_mask"],
        output_names=["text_embeds"],
        dynamic_axes={"input_ids": {0: "queries"}, "attention_mask": {0: "queries"}, "text_embeds": {0: "queries"}},
        opset_version=17,
        dynamo=False,
    )
    query = TextEncoder(model)(tok.input_ids, tok.attention_mask)[None]
    torch.onnx.export(
        Detector(model),
        (pixels, query),
        os.path.join(OUT, "detector.onnx"),
        input_names=["pixel_values", "query_embeds"],
        output_names=["logits", "pred_boxes"],
        dynamic_axes={"query_embeds": {1: "queries"}, "logits": {2: "queries"}},
        opset_version=17,
        dynamo=False,
    )

    # Parity against the reference forward pass.
    ref = model(input_ids=tok.input_ids, attention_mask=tok.attention_mask, pixel_values=pixels)
    import onnxruntime as ort

    text = ort.InferenceSession(os.path.join(OUT, "text_encoder.onnx"))
    det = ort.InferenceSession(os.path.join(OUT, "detector.onnx"))
    emb = text.run(None, {"input_ids": tok.input_ids.numpy(), "attention_mask": tok.attention_mask.numpy()})[0]
    logits, boxes = det.run(None, {"pixel_values": pixels.numpy(), "query_embeds": emb[None]})
    print("embedding dim", dim, "input side", side)
    print("max |logits - ref|", np.abs(logits - ref.logits.numpy()).max())
    print("max |boxes - ref|", np.abs(boxes - ref.pred_boxes.numpy()).max())
