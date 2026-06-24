# Onboarding Code Review — Fix List

Paralel code review sonuçları (3 reviewer, branch `2920ddb..56c1576`).
Sonraki session'da bu dosyayı okuyup SDD ile implement et.

---

## 🔴 Critical / Must Fix

### Fix 1 — `register_mcp_claude` yanlış `is_ok()` kontrolü
**File:** `crates/codi-core/src/init.rs`

`detect_claude_cli` kontrolünde `.status().is_ok()` kullanılıyor.
Bu `Result::Ok`'u kontrol eder, exit code'u değil.
`claude` binary varsa ama non-zero çıkış yaptıysa `true` döner (bug).

```rust
// YANLIŞ (şu an):
.status().is_ok()

// DOĞRU:
.status().map(|s| s.success()).unwrap_or(false)
```

`doctor.rs:150`'de zaten doğru kullanılıyor — aynı pattern'i `init.rs`'e taşı.

---

### Fix 2 — `run_doctor_fix` Turkish UI string üzerinden dispatch ediyor
**File:** `crates/codi-core/src/doctor.rs`

`run_doctor_fix` içinde `check.name` (`"MCP kaydı"`, `".mcp.json"`) fix handler'ını bulmak için kullanılıyor.
UI string değişirse fix sessizce durur, compiler uyarmaz.

**Çözüm:** `CheckResult`'a `id: CheckId` ekle (enum), dispatch buna göre yap.

```rust
#[derive(Debug, PartialEq)]
pub enum CheckId {
    CodiToml,
    Ollama,
    Model,
    McpJson,
    McpRegistration,
    SelfImprovement,
}

pub struct CheckResult {
    pub id: CheckId,
    pub name: &'static str,   // display only
    // ... rest unchanged
}
```

`run_doctor_fix`'te `match check.id { CheckId::McpJson => ..., CheckId::McpRegistration => ... }` kullan.

---

### Fix 3 — `stdout.contains("codi")` substring match fragile
**File:** `crates/codi-core/src/doctor.rs`

`check_claude_mcp_registration`'da `claude mcp list` çıktısı `"codi"` substring'i içeriyor mu diye kontrol ediliyor.
`codi-legacy` gibi başka bir server yanlış pozitif üretebilir.

```rust
// YANLIŞ:
if stdout.contains("codi") {

// DOĞRU — satır başında tam kelime kontrolü:
if stdout.lines().any(|line| {
    let word = line.split_whitespace().next().unwrap_or("");
    word == "codi" || word == "codi:"
}) {
```

---

## 🟠 Important

### Fix 4 — `fill_defaults` ve `detect_ollama` visibility
**Files:** `crates/codi-core/src/init.rs`, `crates/codi-core/src/setup.rs`

- `pub fn fill_defaults` → `pub(crate) fn fill_defaults`
- `pub fn detect_ollama` → `pub(crate) fn detect_ollama`

Bunlar crate-internal API, library surface değil.
Test'ler aynı dosyada `#[cfg(test)]` içinde olduğu için `pub` gerekmez.

---

### Fix 5 — Test boşlukları (en kritik 4 tanesi)

#### 5a — `write_config` merge + model overwrite bug fix'inin testi yok
**File:** `crates/codi-core/src/init.rs` tests

Senaryoyu test et: mevcut `codi.toml`'da `model = "old-model"` var,
`write_config` merge mode'da çağrılıyor `model = "new-model"` ile.
Sonuçta `codi.toml` içinde `new-model` yazmalı (eski `fill_defaults` bug'ı).

```rust
#[test]
fn write_config_merge_overwrites_model_selection() {
    let dir = tempdir().unwrap();
    // Existing toml with old model
    std::fs::write(dir.path().join("codi.toml"), r#"
[model.local]
model = "old-model"
base_url = "http://localhost:11434/v1"
api_key = ""
"#).unwrap();
    write_config(dir.path(), "http://localhost:11434/v1", "new-model", false).unwrap();
    let content = std::fs::read_to_string(dir.path().join("codi.toml")).unwrap();
    assert!(content.contains("new-model"), "model must be overwritten");
    assert!(!content.contains("old-model"), "old model must be gone");
}
```

#### 5b — `doctor_fix_creates_mcp_json` incomplete assertions
**File:** `crates/codi-core/src/doctor.rs` tests

Şu an sadece file existence'ı kontrol ediyor.
`CheckResult.severity` da assert edilmeli:

```rust
#[test]
fn doctor_fix_creates_mcp_json() {
    let dir = tempdir().unwrap();
    let checks = run_doctor_fix(dir.path()).unwrap();
    // File created
    assert!(dir.path().join(".mcp.json").exists());
    // CheckResult updated to Ok
    let mcp = checks.iter().find(|c| c.name == ".mcp.json").unwrap();
    assert!(matches!(mcp.severity, Severity::Ok), "severity must be Ok after fix");
}
```

#### 5c — `no_errors_means_exit_0` tautolojikal
**File:** `crates/codi-core/src/doctor.rs` tests

Şu anki test kendisiyle tutarlılığı kontrol ediyor, davranışı değil.
Gerçek test: sıfır error içeren bilinen bir `checks` slice'ı oluştur.

```rust
#[test]
fn print_doctor_report_returns_false_when_no_errors() {
    let checks = vec![
        CheckResult {
            id: CheckId::CodiToml,
            name: "codi.toml",
            severity: Severity::Ok,
            detail: "mevcut".to_string(),
            suggestion: None,
            fixable: false,
        },
        CheckResult {
            id: CheckId::SelfImprovement,
            name: "self_improvement",
            severity: Severity::Warning,
            detail: "açıkça yapılandırılmamış".to_string(),
            suggestion: None,
            fixable: false,
        },
    ];
    assert!(!print_doctor_report(&checks), "no errors → must return false");
}

#[test]
fn print_doctor_report_returns_true_when_error_present() {
    let checks = vec![CheckResult {
        id: CheckId::McpJson,
        name: ".mcp.json",
        severity: Severity::Error,
        detail: "dosya yok".to_string(),
        suggestion: None,
        fixable: true,
    }];
    assert!(print_doctor_report(&checks), "error present → must return true");
}
```

#### 5d — `ensure_mcp_json` eksik `mcpServers` sub-branch'i test edilmiyor
**File:** `crates/codi-core/src/init.rs` tests

Valid JSON ama `mcpServers` key'i hiç yok senaryosu:

```rust
#[test]
fn ensure_mcp_json_handles_json_without_mcp_servers_key() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".mcp.json"), r#"{"version": 1}"#).unwrap();
    ensure_mcp_json(dir.path()).unwrap();
    let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(json["mcpServers"]["codi"]["command"].as_str() == Some("codi"));
}
```

---

## 🟡 Minor (nice to have, not blocking)

### Minor 1 — Duplike `read_model_from_toml` fonksiyonu
`init.rs::read_model_from_file` ve `doctor.rs::read_model_from_toml` byte-for-byte aynı.
`config.rs`'e veya ayrı bir helper'a taşı, `pub(crate)` yap.

### Minor 2 — `run_doctor` `codi.toml`'u iki kez okuyor
Model adını bir kez oku, yerel değişkende tut.

### Minor 3 — `.mcp.json.bak` tekrarlanan çalıştırmalarda önceki backup'ı eziyor
`create_new(true)` veya timestamp suffix ile koruma eklenebilir.

### Minor 4 — `std::process::exit(1)` Drop'ları atlıyor
`cmd_doctor`'da bu şu an OK (RAII resource yok) ama yorum satırı açıklamalı.

### Minor 5 — `fill_defaults` count semantics
`>= 1` olan test assertion'ı exact count'a çekilebilir.

---

## Uygulama Notu

Bu fix'leri SDD ile implement et:
- **Fix 2** (`CheckId` enum) büyük refactor — önce implement et çünkü Fix 5b/5c testleri buna bağlı
- **Fix 1 + Fix 3** tek commit'te yapılabilir (küçük)
- **Fix 4** (visibility) ayrı commit
- **Fix 5** testleri Fix 2 sonrasında yaz

Plan dosyası buraya yazılacak:
`docs/superpowers/plans/YYYY-MM-DD-onboarding-fixes.md`
