# Ollama storage semantics

Verified on macOS, Ollama at `/usr/local/bin/ollama`, 2026-08-13. Test subject: `all-minilm:22m`.

## Layout

```
~/.ollama/models/                      # override: OLLAMA_MODELS env var
├── blobs/
│   └── sha256-<64 hex>                # content-addressed, flat
└── manifests/
    └── <registry>/<namespace>/<model>/<tag>
        e.g. registry.ollama.ai/library/all-minilm/22m
```

## Manifest format (verified)

OCI image manifest v2 (`application/vnd.docker.distribution.manifest.v2+json`), one JSON
file per tag, no file extension:

```json
{
  "schemaVersion": 2,
  "config": { "mediaType": "application/vnd.docker.container.image.v1+json",
              "digest": "sha256:5484...", "size": 407 },
  "layers": [
    { "mediaType": "application/vnd.ollama.image.model",   "digest": "sha256:797b...", "size": 45949216 },
    { "mediaType": "application/vnd.ollama.image.license", "digest": "sha256:c71d...", "size": 11357 },
    { "mediaType": "application/vnd.ollama.image.params",  "digest": "sha256:8501...", "size": 16 }
  ]
}
```

Other layer media types seen in the wild (not on this machine yet): `.template`,
`.system`, `.adapter`, `.projector`.

## Key facts

- **Blob filename IS the SHA-256 of content.** Verified: `shasum -a 256` of the model
  blob matches its filename digest. Scan can harvest identity with zero hashing;
  verify lazily.
- The `vnd.ollama.image.model` layer is a raw GGUF file — byte-identical to an upstream
  GGUF when the publisher pushed the same bytes. This is the cross-provider dedupe target.
- Ollama already dedupes internally: two tags sharing a layer share one blob.
- Blobs are immutable after pull, mode `0644`. Pull ends with "verifying sha256 digest"
  then "writing manifest" — manifest written last, so a manifest never references a
  missing/partial blob.
- In-progress downloads use partial files inside `blobs/` (`sha256-*-partial*`).
  UNVERIFIED on this machine (pull too fast) — confirm before dedupe skips them.
- `ollama rm <model>` deletes the manifest and any blobs no longer referenced by any
  manifest.

## Dedupe tolerance

- Inference opens blobs read-only (mmap via llama.cpp). Replacing a blob with a
  byte-identical APFS clone while the server is idle is invisible to Ollama.
- Safe window check: no partial file for that digest, server not currently loading it.
- `ollama rm` after dedupe just unlinks Ollama's reference; canonical blob unaffected.

## Open questions

- Does `ollama pull` ever re-verify existing blobs against filename digest (would catch
  a corrupted clone — good for us, but also means a bad swap gets detected loudly)?
- Behavior when a blob has different mtime than manifest write time: none observed.
