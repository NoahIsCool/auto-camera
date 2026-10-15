"""Builds tiny synthetic stand-ins for OWLv2 ONNX exports.

They have the same input/output names and layouts as the real exports
documented in crates/ac-vision/README.md, but 4 "patches", 4-dimensional
embeddings and a 32x32 input, so the `onnx` feature's plumbing (tokenizing,
tensor shapes, output extraction, decoding) is tested without model downloads.

Scores: "person" fires on patches 0 and 3 (3 overlaps 0 and is suppressed by
NMS), "guitar" on patch 1, and "drum" on patch 2 only when the image is bright
(its logit is a gain times the mean normalized pixel), which checks that pixels
reach the graph.

Usage: python make_fixtures.py   (needs `pip install onnx numpy`)
"""

import json
import os

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

HERE = os.path.dirname(os.path.abspath(__file__))
S = 32  # input side
V = 7  # vocabulary size
P = 4  # patches
D = 4  # embedding size
GAIN = 4.0  # drum logit per unit of mean normalized pixel

VOCAB = {"!": 0, "<unk>": 1, "person": 2, "guitar": 3, "drum": 4, "<|startoftext|>": 5, "<|endoftext|>": 6}

# Patch features; the query embedding of token t is one-hot on one dimension.
FEATS = np.array(
    [
        [3.0, -3.0, -5.0, 0.0],
        [-3.0, 3.0, -5.0, 0.0],
        [-3.0, -3.0, 0.0, 0.0],
        [2.5, -3.0, -5.0, 0.0],
    ],
    dtype=np.float32,
)
EMB = np.zeros((V, D), dtype=np.float32)
EMB[VOCAB["person"], 0] = 1.0
EMB[VOCAB["guitar"], 1] = 1.0
EMB[VOCAB["drum"], 2] = 1.0
EMB[VOCAB["<unk>"], 3] = 1.0
# Brightness feeds dimension 2 (drum) of patch 2 only.
BRIGHT = np.zeros((1, P, D), dtype=np.float32)
BRIGHT[0, 2, 2] = GAIN
BOXES = np.array(
    [[[0.25, 0.15, 0.2, 0.2], [0.75, 0.15, 0.2, 0.2], [0.5, 0.3, 0.2, 0.2], [0.26, 0.15, 0.2, 0.2]]],
    dtype=np.float32,
)


def const(name, array):
    return helper.make_node("Constant", [], [name], value=numpy_helper.from_array(array, name + "_value"))


def save(graph, name):
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)], producer_name="ac-vision-fixtures")
    model.ir_version = 8
    onnx.checker.check_model(model)
    onnx.save(model, os.path.join(HERE, name))


def first_token_nodes():
    """input_ids [Q, L] -> tok [Q] (the token after BOS)."""
    return [
        const("one", np.array(1, dtype=np.int64)),
        helper.make_node("Gather", ["input_ids", "one"], ["tok"], axis=1),
    ]


def feature_nodes():
    """pixel_values -> feats [1, P, D] with the brightness term added."""
    return [
        const("feats_base", FEATS[None]),
        const("bright", BRIGHT),
        helper.make_node("ReduceMean", ["pixel_values"], ["mean"], keepdims=0),
        helper.make_node("Mul", ["bright", "mean"], ["bright_term"]),
        helper.make_node("Add", ["feats_base", "bright_term"], ["feats"]),
    ]


def outputs():
    return [
        helper.make_tensor_value_info("logits", TensorProto.FLOAT, [1, P, "q"]),
        helper.make_tensor_value_info("pred_boxes", TensorProto.FLOAT, [1, P, 4]),
    ]


text_inputs = [
    helper.make_tensor_value_info("input_ids", TensorProto.INT64, ["q", "l"]),
    helper.make_tensor_value_info("attention_mask", TensorProto.INT64, ["q", "l"]),
]
pixel_input = helper.make_tensor_value_info("pixel_values", TensorProto.FLOAT, [1, 3, S, S])
box_node = const("pred_boxes", BOXES)

# Combined: ids + pixels -> logits.
combined = helper.make_graph(
    first_token_nodes()
    + feature_nodes()
    + [
        const("emb", EMB),
        helper.make_node("Gather", ["emb", "tok"], ["q_emb"], axis=0),  # [Q, D]
        helper.make_node("Transpose", ["q_emb"], ["q_emb_t"], perm=[1, 0]),  # [D, Q]
        helper.make_node("MatMul", ["feats", "q_emb_t"], ["logits"]),  # [1, P, Q]
        box_node,
    ],
    "owl_combined",
    text_inputs + [pixel_input],
    outputs(),
)
save(combined, "combined.onnx")

# Split: text encoder.
text = helper.make_graph(
    first_token_nodes() + [const("emb", EMB), helper.make_node("Gather", ["emb", "tok"], ["text_embeds"], axis=0)],
    "owl_text",
    text_inputs,
    [helper.make_tensor_value_info("text_embeds", TensorProto.FLOAT, ["q", D])],
)
save(text, "text_encoder.onnx")

# Split: detector taking query embeddings.
detector = helper.make_graph(
    feature_nodes()
    + [
        helper.make_node("Transpose", ["query_embeds"], ["q_t"], perm=[0, 2, 1]),  # [1, D, Q]
        helper.make_node("MatMul", ["feats", "q_t"], ["logits"]),
        box_node,
    ],
    "owl_detector",
    [pixel_input, helper.make_tensor_value_info("query_embeds", TensorProto.FLOAT, [1, "q", D])],
    outputs(),
)
save(detector, "detector.onnx")

SPECIAL = ["<|startoftext|>", "<|endoftext|>"]
tokenizer = {
    "version": "1.0",
    "truncation": None,
    "padding": None,
    "added_tokens": [
        {
            "id": VOCAB[t],
            "content": t,
            "single_word": False,
            "lstrip": False,
            "rstrip": False,
            "normalized": False,
            "special": True,
        }
        for t in SPECIAL
    ],
    "normalizer": {"type": "Lowercase"},
    "pre_tokenizer": {"type": "Whitespace"},
    "post_processor": {
        "type": "TemplateProcessing",
        "single": [
            {"SpecialToken": {"id": "<|startoftext|>", "type_id": 0}},
            {"Sequence": {"id": "A", "type_id": 0}},
            {"SpecialToken": {"id": "<|endoftext|>", "type_id": 0}},
        ],
        "pair": [{"Sequence": {"id": "A", "type_id": 0}}, {"Sequence": {"id": "B", "type_id": 1}}],
        "special_tokens": {t: {"id": t, "ids": [VOCAB[t]], "tokens": [t]} for t in SPECIAL},
    },
    "decoder": None,
    "model": {"type": "WordLevel", "vocab": VOCAB, "unk_token": "<unk>"},
}
with open(os.path.join(HERE, "tokenizer.json"), "w") as f:
    json.dump(tokenizer, f, indent=1)
    f.write("\n")
