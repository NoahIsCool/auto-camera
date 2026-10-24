# ADR 0001: Workspace layout

Status: accepted (2026-10-14)

## Context

The proof of concept grew into one 3,000-line orchestrator with shared locks,
media work in Python, and a README used as the design document. The rebuild
needs small, independently tested pieces with one owner per piece of state.

## Decision

A Cargo workspace with pure, deterministic core crates and thin apps:

| Crate | Owns |
|---|---|
| `ac-core` | Domain model (sources, queries, subjects, presets, shots) and normalized geometry |
| `ac-framing` | Ideal-crop composition and the per-shot smoothing controller |
| `ac-switcher` | Preview/program state, cut/auto/T-bar transitions |
| `ac-eval` | Framing-quality metrics used to gate releases |
| `apps/auto-camera` | The executable that wires the crates together |

Capture, inference, compositing, output and audio arrive as further crates
behind traits, so model runtimes (ONNX Runtime, Burn) and platforms can be
swapped without touching the core.

## Consequences

Core crates have no I/O and no clocks; time enters as `dt`. That keeps them
testable offline against recorded observations and annotated footage.
