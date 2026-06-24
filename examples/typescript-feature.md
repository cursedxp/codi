# Example: Working on a TypeScript/Node project

```
# codi.toml for a pnpm/Next.js project
[model.local]
base_url = "http://localhost:11434/v1"
model    = "qwen2.5-coder:7b"

[commands]
test   = "pnpm test"
lint   = "pnpm lint"
build  = "pnpm build"
format = "pnpm format"

[rag]
extensions = ["ts", "tsx", "js", "jsx", "json", "md"]
```

```
# Index the repo
codi index

# Ask for a feature
codi run "Add a /api/healthz endpoint that returns { status: 'ok', timestamp: <ISO date> }"

# Hybrid mode: simple tasks stay local, complex ones escalate to Claude
# (requires [model.cloud] + routing.mode = "hybrid" in codi.toml)
codi run "Refactor the auth middleware across all API routes to use the new JWT library"
```
