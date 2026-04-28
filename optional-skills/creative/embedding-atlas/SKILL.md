---
name: embedding-atlas
description: Build and inspect large embedding visualizations with Apple's Embedding Atlas (Python CLI, notebook widget, and npm component modes). Use when users want interactive embedding maps, nearest-neighbor exploration, metadata cross-filtering, or shareable visual analytics on vector datasets.
version: 1.0.0
author: Hermes Agent Ultra
license: MIT
metadata:
  hermes:
    tags: [embedding, visualization, vector, analytics, python, notebook, webgpu, apple]
    category: creative
---

# embedding-atlas

Apple's `embedding-atlas` provides interactive visualizations for large embedding
datasets with metadata filtering, nearest-neighbor lookup, and fast WebGPU-first
rendering (WebGL2 fallback).

Repo: https://github.com/apple/embedding-atlas  
Docs/demo: https://apple.github.io/embedding-atlas

## When to use this skill

- User wants to visualize embedding clusters and neighborhoods interactively
- User has tabular/Parquet embedding data and needs exploratory analysis
- User wants notebook-native embedding maps (Jupyter widget) or web embed
- User needs metadata cross-filtering tied to embedding geometry

## Workflow fit (terminal-first)

This skill is intentionally terminal-native and works well with Hermes tools:

1. Use `search_files`/`read_file`/`terminal` to locate and inspect embedding data.
2. Normalize dataset schema.
3. Launch atlas via CLI or notebook.
4. Export/share artifacts and summarize findings.

## Required data shape

Embedding Atlas expects embedding vectors plus optional metadata columns.
The most reliable path is a Parquet file with at least:

- `embedding` (vector column)
- one identifier column (for example `id`)
- optional metadata columns (labels, source, timestamp, etc.)

## Quick start (Python CLI)

```bash
python3 -m pip install --upgrade embedding-atlas pyarrow pandas
embedding-atlas /path/to/your-dataset.parquet
```

If no browser opens automatically, copy the printed local URL.

## Notebook widget mode

```python
import pandas as pd
from embedding_atlas.widget import EmbeddingAtlasWidget

df = pd.read_parquet("your-dataset.parquet")
EmbeddingAtlasWidget(df)
```

## Frontend integration (npm)

```bash
npm install embedding-atlas
```

```ts
import { EmbeddingAtlas, EmbeddingView } from "embedding-atlas";
// React: from "embedding-atlas/react"
// Svelte: from "embedding-atlas/svelte"
```

## Common data-prep pattern

```python
import pandas as pd
import numpy as np

df = pd.read_parquet("raw.parquet")

# Example: ensure embedding column is list-like float vectors
df["embedding"] = df["embedding"].apply(
    lambda v: np.asarray(v, dtype=np.float32).tolist()
)

df.to_parquet("atlas_ready.parquet", index=False)
```

## Troubleshooting

- If launch fails: verify Python env and `pyarrow` install.
- If rendering is slow: downsample first, then inspect full set by slice.
- If vectors are rejected: enforce uniform vector length and float dtype.
- If browser rendering fails: update browser/GPU drivers; WebGL fallback should engage.

## Suggested Hermes execution pattern

1. Validate input file schema.
2. Build `atlas_ready.parquet`.
3. Launch atlas.
4. Save session notes (clusters, outliers, nearest-neighbor anomalies) to `MEMORY.md`/report files.

