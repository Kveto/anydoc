# Describe embedded images with local Ollama

Install Ollama, start its local service, and make sure `curl` is on your PATH. From the Anydoc source directory, run:

```sh
ollama pull gemma3:4b
cargo run --locked --example ollama_images -- report.docx report-output
```

Use a new output directory. Pass an installed local vision model as the optional third argument.

```sh
cargo run --locked --example ollama_images -- report.docx report-output qwen3.5:9b
```

Open `report-output/document.md`. Descriptions appear where the images occurred. The extracted images section links their IDs to the original files under `assets/`. Identical visual payloads are described once per run. Anydoc retains its existing table rendering.

The example sends embedded PNG, JPEG, and WebP images one at a time to Ollama on loopback. Use a local Ollama service and a model installed for local inference. Requests have a timeout and a 16 MiB encoded-source-image size limit before base64 encoding. If an image description fails, the output records that and retains the saved image. External, unsupported, and missing payloads retain their existing markers. PDF pages requiring OCR retain their existing markers as well. The example performs image description. PDF rasterization and a dedicated OCR pass are separate steps.
