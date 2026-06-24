# CLAUDE.md Delegasyon Talimatı Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `codi init` projesinin kökünde bir `CLAUDE.md` oluşturarak Claude'a implementasyon işlerini codi MCP araçlarına devretmesini söyler; `codi doctor` bunu kontrol eder ve `--fix` ile düzeltir.

**Architecture:** `ensure_claude_md` fonksiyonu `init.rs`'e eklenir, `ensure_mcp_json` ile aynı 3-state merge mantığını kullanır (yok → oluştur, var+bölüm yok → ekle, var+bölüm var → dokunma). `doctor.rs`'e yeni `CheckId::ClaudeMd` ve `check_claude_md` helper eklenir; `run_doctor_fix` bu check'i ele alır.

**Tech Stack:** Rust, `std::fs`, `std::io::Write` (append), `tempfile::tempdir` (testler için).

## Global Constraints

- Tüm kullanıcı çıktısı `println!` ile — `eprintln!` kullanma.
- Türkçe UI metinleri korunur (mevcut pattern ile tutarlı).
- `ensure_claude_md` → `pub(crate)` görünürlük (`ensure_mcp_json` ile aynı).
- Mevcut CLAUDE.md içeriği asla silinmez veya değiştirilmez.
- `CLAUDE_MD_MARKER = "## codi"` — varlık kontrolü için.
- TDD: önce test yaz, sonra implement et.

---

## Dosya Haritası

| Dosya | Değişiklik |
|-------|-----------|
| `crates/codi-core/src/init.rs` | `CLAUDE_MD_SECTION` + `CLAUDE_MD_MARKER` sabit, `ensure_claude_md` fonksiyon, `run_init` adım numaraları güncelle (`[1/5]`→`[1/6]`…), `[6/6]` adımı ekle, testler |
| `crates/codi-core/src/doctor.rs` | `CheckId::ClaudeMd` enum varyantı, `check_claude_md` helper, `run_doctor` çağrısı, `run_doctor_fix` arm, testler |

---

## Task 1: `ensure_claude_md` — init.rs

**Files:**
- Modify: `crates/codi-core/src/init.rs`

**Interfaces:**
- Produces: `pub(crate) fn ensure_claude_md(repo_root: &Path) -> Result<()>` — Task 2'nin `run_doctor_fix`'i bu imzayı çağırır.

---

- [ ] **Step 1: Failing testleri yaz**

`crates/codi-core/src/init.rs` içinde mevcut `#[cfg(test)] mod tests` bloğuna şu testleri ekle:

```rust
#[test]
fn ensure_claude_md_creates_file_when_absent() {
    let dir = tempdir().unwrap();
    ensure_claude_md(dir.path()).unwrap();
    let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
    assert!(content.contains("## codi"), "must contain ## codi marker");
    assert!(content.contains("run_task"), "must mention run_task");
}

#[test]
fn ensure_claude_md_appends_section_when_marker_absent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    std::fs::write(&path, "# My Project\n\nExisting content.\n").unwrap();
    ensure_claude_md(dir.path()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.starts_with("# My Project"), "existing content preserved");
    assert!(content.contains("## codi"), "codi section appended");
}

#[test]
fn ensure_claude_md_leaves_existing_intact_when_marker_present() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    let original = "# My Project\n\n## codi — AI coding agent\n\nCustom content.\n";
    std::fs::write(&path, original).unwrap();
    ensure_claude_md(dir.path()).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, original, "file must be byte-identical");
}
```

- [ ] **Step 2: Testlerin fail ettiğini doğrula**

```bash
cargo test -p codi-core ensure_claude_md 2>&1 | tail -20
```

Beklenen: `error[E0425]: cannot find function 'ensure_claude_md'`

- [ ] **Step 3: Sabitleri ve `ensure_claude_md` fonksiyonunu ekle**

`init.rs` dosyasının en üstündeki mevcut `const` satırlarının hemen altına ekle:

```rust
const CLAUDE_MD_MARKER: &str = "## codi";
const CLAUDE_MD_SECTION: &str = "\n\n## codi \u{2014} AI coding agent\n\nBu projede m\u{00fc}mk\u{00fc}n oldu\u{011f}unda do\u{011f}rudan kod yazmak yerine codi MCP ara\u{00e7}lar\u{0131}n\u{0131}\ntercih et:\n\n- **Kod yaz / refactor / bug fix** \u{2192} `run_task` ile codi'ye devret\n- **De\u{011f}i\u{015f}iklikleri incele** \u{2192} `get_diff` ile diff al, sen yorumla\n- **Testleri do\u{011f}rula** \u{2192} `run_tests` ile \u{00e7}al\u{0131}\u{015f}t\u{0131}r\n\nClaude'un rol\u{00fc}: planlama, review, g\u{00fc}venlik, koordinasyon.\ncodi'nin rol\u{00fc}: implementasyon (yerel LLM \u{00fc}zerinden Goose ile \u{00e7}al\u{0131}\u{015f}\u{0131}r).\n";
```

Ardından `register_mcp_claude` fonksiyonunun hemen öncesine yeni fonksiyonu ekle:

```rust
pub(crate) fn ensure_claude_md(repo_root: &Path) -> Result<()> {
    let path = repo_root.join("CLAUDE.md");

    if !path.exists() {
        let content = CLAUDE_MD_SECTION.trim_start_matches('\n');
        std::fs::write(&path, content).context("writing CLAUDE.md")?;
        println!("  \u{2713} CLAUDE.md olu\u{015f}turuldu");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path).context("reading CLAUDE.md")?;
    if content.contains(CLAUDE_MD_MARKER) {
        println!("  \u{2713} CLAUDE.md \u{2014} de\u{011f}i\u{015f}tirilmedi");
        return Ok(());
    }

    use std::io::Write as IoWrite;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .context("opening CLAUDE.md for append")?;
    file.write_all(CLAUDE_MD_SECTION.as_bytes()).context("appending to CLAUDE.md")?;
    println!("  \u{2713} CLAUDE.md \u{2014} codi b\u{00f6}l\u{00fc}m\u{00fc} eklendi");
    Ok(())
}
```

- [ ] **Step 4: `run_init` adım numaralarını güncelle ve [6/6] ekle**

`run_init` içinde şu değişiklikleri yap:

```rust
// [1/5] → [1/6]
println!("[1/6] Ollama kontrolü");

// [2/5] → [2/6]
println!("[2/6] Model seçimi");

// [3/5] → [3/6]
println!("[3/6] codi.toml");

// [4/5] → [4/6]
println!("[4/6] .mcp.json");

// [5/5] → [5/6]
println!("[5/6] MCP kaydı");
```

Son `register_mcp_claude();` satırının hemen ardına ekle:

```rust
// [6/6] CLAUDE.md
println!("[6/6] CLAUDE.md");
ensure_claude_md(repo_root)?;
```

- [ ] **Step 5: Testleri çalıştır**

```bash
cargo test -p codi-core ensure_claude_md 2>&1 | tail -15
```

Beklenen: `3 passed`

- [ ] **Step 6: Tüm codi-core testlerini çalıştır**

```bash
cargo test -p codi-core 2>&1 | tail -10
```

Beklenen: tüm testler pass, 0 failed.

- [ ] **Step 7: Build doğrula**

```bash
cargo build -p codi-cli 2>&1 | tail -5
```

Beklenen: `Finished` — warning veya error yok.

- [ ] **Step 8: Commit**

```bash
git add crates/codi-core/src/init.rs
git commit -m "feat(init): add CLAUDE.md delegation instruction as step 6/6"
```

---

## Task 2: `ClaudeMd` check — doctor.rs

**Files:**
- Modify: `crates/codi-core/src/doctor.rs`

**Interfaces:**
- Consumes: `pub(crate) fn ensure_claude_md(repo_root: &Path) -> Result<()>` — Task 1'den.
- `CLAUDE_MD_MARKER` sabitine doğrudan erişim yok; doctor.rs kendi `"## codi"` literal'ini kullanır (aynı değer, bağımlılık yaratmaya gerek yok).

---

- [ ] **Step 1: Failing testleri yaz**

`doctor.rs` içinde mevcut `#[cfg(test)] mod tests` bloğuna şu testleri ekle:

```rust
#[test]
fn check_claude_md_missing_returns_error() {
    let dir = tempdir().unwrap();
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
    assert!(matches!(c.severity, Severity::Error));
    assert!(c.fixable);
}

#[test]
fn check_claude_md_without_codi_section_returns_error() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "# My Project\n").unwrap();
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
    assert!(matches!(c.severity, Severity::Error));
    assert!(c.fixable);
}

#[test]
fn check_claude_md_with_codi_section_returns_ok() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "# Project\n\n## codi\n\nContent.\n").unwrap();
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
    assert!(matches!(c.severity, Severity::Ok));
}

#[test]
fn doctor_fix_creates_claude_md_and_marks_ok() {
    let dir = tempdir().unwrap();
    let checks = run_doctor_fix(dir.path()).unwrap();
    assert!(dir.path().join("CLAUDE.md").exists(), "CLAUDE.md must be created");
    let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd)
        .expect("ClaudeMd check must be present");
    assert!(matches!(c.severity, Severity::Ok), "severity must be Ok after fix");
}
```

- [ ] **Step 2: Testlerin fail ettiğini doğrula**

```bash
cargo test -p codi-core check_claude_md 2>&1 | tail -20
```

Beklenen: `error[E0599]: no variant ... ClaudeMd`

- [ ] **Step 3: `CheckId::ClaudeMd` enum varyantını ekle**

`doctor.rs` içindeki `CheckId` enum'una son varyant olarak ekle:

```rust
#[derive(Debug, PartialEq)]
pub enum CheckId {
    CodiToml,
    Ollama,
    Model,
    McpJson,
    McpRegistration,
    SelfImprovement,
    ClaudeMd,           // ← ekle
}
```

- [ ] **Step 4: `check_claude_md` helper fonksiyonunu ekle**

`doctor.rs` dosyasının sonundaki `fn read_model_from_toml` fonksiyonunun hemen öncesine ekle:

```rust
fn check_claude_md(repo_root: &Path) -> CheckResult {
    let path = repo_root.join("CLAUDE.md");
    if !path.exists() {
        return CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Error,
            detail: "dosya yok".to_string(),
            suggestion: Some("codi doctor --fix".to_string()),
            fixable: true,
        };
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if content.contains("## codi") {
        CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Ok,
            detail: "codi delegasyon talimat\u{0131} mevcut".to_string(),
            suggestion: None,
            fixable: false,
        }
    } else {
        CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Error,
            detail: "codi b\u{00f6}l\u{00fc}m\u{00fc} eksik".to_string(),
            suggestion: Some("codi doctor --fix".to_string()),
            fixable: true,
        }
    }
}
```

- [ ] **Step 5: `run_doctor` içine check çağrısını ekle**

`run_doctor` içinde `// [5] MCP registration` ve `// [6] self_improvement` check'lerinin ardına — yani `Ok(checks)` satırından hemen önce — ekle:

```rust
    // [7] CLAUDE.md
    checks.push(check_claude_md(repo_root));
```

- [ ] **Step 6: `run_doctor_fix` içine `ClaudeMd` arm ekle**

`run_doctor_fix` içindeki `match check.id` bloğuna `McpRegistration` arm'ından sonra ekle:

```rust
            CheckId::ClaudeMd => {
                match crate::init::ensure_claude_md(repo_root) {
                    Ok(()) => {
                        check.severity = Severity::Ok;
                        check.detail = "d\u{00fc}zeltildi".to_string();
                        check.suggestion = None;
                    }
                    Err(e) => {
                        check.detail = format!("d\u{00fc}zeltme ba\u{015f}ar\u{0131}s\u{0131}z: {e:#}");
                    }
                }
            }
```

- [ ] **Step 7: Testleri çalıştır**

```bash
cargo test -p codi-core check_claude_md doctor_fix_creates_claude_md 2>&1 | tail -15
```

Beklenen: `4 passed`

- [ ] **Step 8: Tüm codi-core testlerini çalıştır**

```bash
cargo test -p codi-core 2>&1 | tail -10
```

Beklenen: tüm testler pass, 0 failed.

- [ ] **Step 9: Build doğrula**

```bash
cargo build -p codi-cli 2>&1 | tail -5
```

Beklenen: `Finished` — warning veya error yok.

- [ ] **Step 10: Commit**

```bash
git add crates/codi-core/src/doctor.rs
git commit -m "feat(doctor): add ClaudeMd check and --fix support"
```
