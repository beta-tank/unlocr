# Unlimited-OCR-2 llama-server

`unlimited-ocr2.patch` applies to the official `ggml-org/llama.cpp` release
**b11913**, commit `7aca0d2ea8d92b56911233b822db01f9fbb1ae50`.

The patch is sourced from
[RX5950XT/Unlimited-OCR-2-GGUF on Hugging Face](https://huggingface.co/RX5950XT/Unlimited-OCR-2-GGUF).

Run `.github/workflows/build-llama-ocr2.yml` manually with a fresh `release_tag`,
for example `llama-ocr2-b11913-20261109`. It checks the upstream commit, applies
the patch, and builds only `llama-server` and its dependencies for Linux x64,
Windows x64, macOS arm64 (Metal), and macOS x64 (CPU).

The build disables OpenSSL (`-DLLAMA_OPENSSL=OFF`), tests, examples, the unified
app, and both building and downloading the server Web UI. Libraries are linked
statically; macOS arm64 embeds its Metal shaders. Each platform runs
`llama-server --version` before publishing a ZIP containing only the executable,
a `.zip.sha256` checksum, and a build provenance attestation to a prerelease.

The workflow records the upstream commit and the unlocr revision containing the
patch in the build summary/release metadata. Use a new tag for every rebuild so
published ZIPs and their checksums remain immutable.

For local verification after cloning b11913 and applying the patch:

```sh
cmake -S llama.cpp -B llama.cpp/build \
  -DCMAKE_BUILD_TYPE=Release \
  -DLLAMA_BUILD_NUMBER=11913 \
  -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TOOLS=ON \
  -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
  -DLLAMA_BUILD_APP=OFF -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF \
  -DLLAMA_OPENSSL=OFF -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF
cmake --build llama.cpp/build --config Release --target llama-server -j
```

The patch currently supports one sequence: use `-np 1` when loading an
Unlimited-OCR-2 model. The workflow's version check verifies executable startup;
OCR inference still needs a converted GGUF model and its vision projector.
