# Xybrid

Xybrid keeps local models under `~/.xybrid`, including `cache/models/*/*.xyb`
bundles and extracted weights in `cache/extracted` and `cache/hf`. ModelFS scans
GGUF, safetensors, ONNX, PyTorch, and XYB files using the plain-file scanner.
XYB bundles are opaque: identity is their computed digest, with no header parser.

`cache/precompiled`, `cache/xcframework`, `cache/ort-ios`, and `traces` hold
runtime/toolchain files or logs and are excluded. Metadata, checksum sidecars,
and symlinked files are not imported. The daemon waits for model downloads to
settle before importing them, as it does for the other providers.

Removal moves only selected model files to the Trash. Cache folders and sidecar
state stay in place; there is no provider API or whole-folder deletion.
