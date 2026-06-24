# codi Onboarding Design

## Goal

Collapse onboarding from a multi-step manual process into two commands:
`codi init` (single setup entry point) and `codi doctor` / `codi doctor --fix` (health check + safe auto-fix).
Remove the auto-wizard that triggers on first launch; replace with a clear onboarding prompt.

---

## Scope

**In scope:**
- `codi init` — project-level onboarding command
- `codi doctor` — read-only health check
- `codi doctor --fix` — safe auto-fix (project-level only)
- Reuse of existing `setup.rs` functions
- Removal of auto-wizard trigger from `main.rs`
- First-launch messaging (onboarding-style, not error-style)

**Out of scope:**
- Installer script
- Homebrew formula
- System-level auto-fix (Goose install, Ollama install, model downloads)

---

## Architecture

### New files

- `crates/codi-core/src/init.rs` — `codi init` logic
- `crates/codi-core/src/doctor.rs` — `codi doctor` / `codi doctor --fix` logic

### Modified files

- `crates/codi-core/src/lib.rs` — add `pub mod init; pub mod doctor;`
- `crates/codi-core/src/setup.rs` — keep all functions `pub`; `detect_ollama` exposed as `pub`
- `crates/codi-cli/src/main.rs` — remove auto-wizard trigger; add `Init` and `Doctor` subcommands

### Reused from `setup.rs`

| Function | Used by |
|----------|---------|
| `detect_ollama()` | `init.rs` step 1, `doctor.rs` check 2 |
| `pick_model_interactive()` | `init.rs` step 2 (when no model or model not installed) |
| `is_first_launch()` | `main.rs` first-launch message only |

---

## `codi init`

### Entry point

```
codi init
codi init --rewrite-config
```

### Step-by-step flow

```
[1/5] Ollama check
[2/5] Model selection
[3/5] codi.toml merge
[4/5] .mcp.json
[5/5] MCP registration
```

#### Step 1 — Ollama check

Calls `detect_ollama()`. If Ollama is not reachable:

```
✗ Ollama bulunamadı. Kur: brew install ollama && ollama serve
```

Aborts with non-zero exit. Does not write any files.

#### Step 2 — Model selection

Reads model from existing `codi.toml` (if present). Before accepting it:

- Queries Ollama `/api/tags` to verify the model is installed locally.
- If installed → prints `✓ qwen2.5:7b mevcut — korunuyor` and proceeds.
- If not installed → prints `⚠ qwen2.5:7b Ollama'da yüklü değil` and opens `pick_model_interactive()`.
- If no existing config or `--rewrite-config` → opens `pick_model_interactive()`.

#### Step 3 — codi.toml

**Default mode (merge):**

- If `codi.toml` exists: parse it, add any missing top-level sections with sensible defaults, write back.
- If `codi.toml` does not exist: write from `Config::default()`.
- Existing fields are never overwritten.
- Deprecated or unknown fields trigger a `⚠` warning but are preserved (not removed).
- Prints: `✓ codi.toml — 3 eksik alan eklendi` (or `oluşturuldu`).

**`--rewrite-config` mode:**

- Deletes existing `codi.toml` and writes a fresh file from scratch.
- Prints: `✓ codi.toml yeniden oluşturuldu`.

#### Step 4 — `.mcp.json`

- If `.mcp.json` does not exist: create it with the codi entry.
- If `.mcp.json` exists and is valid JSON:
  - Check whether a `codi` entry is present under `mcpServers`.
  - If missing: add the entry, write back. Print `✓ .mcp.json — codi kaydı eklendi`.
  - If present: leave untouched. Print `✓ .mcp.json — değiştirilmedi`.
- If `.mcp.json` exists but is malformed (not valid JSON):
  - Save backup as `.mcp.json.bak`.
  - Overwrite with a fresh file containing the codi entry.
  - Print `⚠ .mcp.json bozuktu — .mcp.json.bak olarak yedeklendi, yeniden oluşturuldu`.

`.mcp.json` content:

```json
{
  "mcpServers": {
    "codi": {
      "command": "codi",
      "args": ["mcp"]
    }
  }
}
```

#### Step 5 — MCP registration

Runs `claude mcp add codi -- codi mcp`.

- `claude` CLI not found on PATH:
  ```
  [ℹ] MCP kaydı atlandı — claude CLI yüklü değil.
      Manuel kayıt: claude mcp add codi -- codi mcp
  ```
  (Info level — not an error, not a warning. Does not affect exit code.)

- `claude` found but command exits non-zero:
  ```
  ⚠ MCP kaydı başarısız.
      Manuel kayıt: claude mcp add codi -- codi mcp
  ```

- Success:
  ```
  ✓ MCP kaydı yapıldı (claude mcp add codi)
  ```

### Completion summary

```
Tamamlandı. Şimdi Claude Code'u bu projede açıp kullanmaya başlayabilirsin.
```

### Idempotency

`codi init` is safe to run multiple times. Every step checks existing state before acting. Running it twice on a configured project produces no changes and exits `0`.

---

## `codi doctor`

### Entry point

```
codi doctor
codi doctor --fix
```

### Check list

| # | Check | Condition | Severity |
|---|-------|-----------|----------|
| 1 | `codi.toml` | file missing | `[✗]` |
| 2 | Ollama | not reachable | `[✗]` |
| 3 | Model installed | model in config not in Ollama `/api/tags` | `[✗]` |
| 4 | `.mcp.json` | file missing or invalid JSON | `[✗]` |
| 5 | MCP registration | `claude` CLI absent | `[ℹ]` info (not error) |
| 5 | MCP registration | `claude` present but codi not registered | `[✗]` |
| 6 | `[self_improvement]` | section absent from config | `[⚠]` |

### Output format

```
codi doctor — project health check

[✓] codi.toml           mevcut (model: qwen2.5:7b)
[✓] Ollama              çalışıyor — model yüklü
[✗] .mcp.json           dosya yok
    → Oluşturmak için: codi doctor --fix
[✗] MCP kaydı           codi kayıtlı değil
    → Düzeltmek için: codi doctor --fix
[⚠] self_improvement    config yok — varsayılan devre dışı
    → codi.toml'a [self_improvement] ekle

2 sorun bulundu, 1 uyarı.
Otomatik düzeltilebilir: codi doctor --fix
```

### Exit codes

| Situation | Exit code |
|-----------|-----------|
| All checks pass (or only `[⚠]` and `[ℹ]`) | `0` |
| At least one `[✗]` | `1` |

### `codi doctor --fix`

Runs all checks, then applies safe auto-fixes:

**Fixes applied automatically:**

| Issue | Fix |
|-------|-----|
| `.mcp.json` missing | Create with codi entry |
| `.mcp.json` malformed | Backup to `.mcp.json.bak`, recreate |
| `.mcp.json` present but codi entry missing | Add codi entry |
| `claude` present, codi not registered | Run `claude mcp add codi -- codi mcp` (soft-fail) |

**Not auto-fixed (requires manual action):**

| Issue | Reason |
|-------|--------|
| `codi.toml` missing | Run `codi init` instead |
| Ollama not running | System-level; print `ollama serve` suggestion |
| Model not installed | System-level; print `ollama pull <model>` suggestion |
| `[self_improvement]` absent | User preference; print config snippet suggestion |
| `claude` CLI absent | Not installable by codi; print `[ℹ]` info only |

### `.mcp.json.bak` backup

When `--fix` overwrites a malformed `.mcp.json`:

1. Write current content to `.mcp.json.bak` (overwriting any prior `.bak`).
2. Write fresh `.mcp.json`.
3. Print: `⚠ .mcp.json bozuktu — .mcp.json.bak olarak yedeklendi, yeniden oluşturuldu`.

---

## First-launch behavior (auto-wizard removal)

The existing `is_first_launch()` check in `main.rs` currently triggers `first_launch_wizard()` automatically. This auto-trigger is removed.

**New behavior:**

When `is_first_launch()` returns true (no `codi.toml`, no user config), `main.rs` prints an onboarding prompt and exits:

```
Bu proje henüz yapılandırılmamış. Başlamak için:

  codi init

```

This message is styled as guidance, not an error. It does not use `eprintln!` and does not exit with a non-zero code.

The `first_launch_wizard()` function remains in `setup.rs` and is still callable directly by `codi init`.

---

## Severity levels

| Symbol | Name | Meaning | Affects exit code |
|--------|------|---------|-------------------|
| `[✓]` | OK | Check passed | No |
| `[✗]` | Error | Problem that blocks normal use | Yes (exit 1) |
| `[⚠]` | Warning | Suboptimal but functional | No |
| `[ℹ]` | Info | Neutral context, no action needed | No |

`claude` CLI absence is always `[ℹ]` — codi works without it; MCP integration is optional.

---

## Config merge algorithm (detail)

When `codi init` merges into an existing `codi.toml`:

1. Parse existing file into a `toml::Value` table (not into `Config` struct — preserves unknown keys).
2. For each top-level section in `Config::default()` (`model`, `routing`, `commands`, `rag`, `safety`, `self_improvement`):
   - If section is absent in existing file: insert the default section.
   - If section is present: recurse into keys. Insert missing keys with defaults; leave present keys untouched.
3. Check for keys present in the file but absent from `Config` — print `⚠ bilinmeyen alan: <key>` for each.
4. Serialize back with `toml::to_string_pretty` and write.

This means `codi init` on a file that only has `[model.local]` will add `[routing]`, `[commands]`, `[rag]`, `[safety]`, and `[self_improvement]` with defaults, while leaving the existing `[model.local]` fields untouched.
