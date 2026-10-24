# Auto Camera

AI camera crew for live streams: text prompts pick subjects, each subject gets a
smoothly framed virtual shot, and an operator switches shots in a preview/program
(A/B) setup.

Status: early rebuild. The core domain model, framing controller and switcher
exist with tests; capture, inference, compositing and output are next. Design
decisions live in [`docs/adr/`](docs/adr/).

## Develop

Requires Rust 1.92 or newer.

```sh
cargo test                                   # all unit tests
cargo clippy --all-targets -- -D warnings    # lints
cargo run -p auto-camera                     # synthetic framing + switching rehearsal
```
