# Codi Reliability Layer — Design Spec
**Date:** 2026-06-25  
**Status:** Approved

---

## Problem

Küçük lokal modeller (özellikle 7B sınıfı) büyük, çok dosyalı görevlerde güvenilir çalışmıyor:

- Multi-file görevlerde model görevi yanlış parse ediyor
- exit_code=0 dönüyor ama beklenen dosyalar oluşmuyor ("sessiz başarısızlık")
- Küçük, tek dosya odaklı görevlerde başarı oranı belirgin şekilde daha yüksek
- Başarısızlık sonrası sistem nedenini anlayamıyor, aynı hatayı tekrarlıyor

---

## Hedefler

1. Büyük multi-file görevleri doğrudan küçük modele vermeme
2. exit_code=0 + diff boşsa explicit failure say ("sessiz başarısızlık" yok)
3. Başarısız görevlerde retry → cloud escalation → explicit error zinciri
4. Kararların (neden bölündü, neden retry edildi, neden escalate edildi) loglanması
5. `codi doctor` üzerinden reliability görünürlüğü

---

## Kısıtlar

- `engine.rs` dokunulmaz kalır — sadece execution primitive'i
- Cloud gereksiz yere kullanılmaz; sadece retry başarısız olunca devreye girer
- `enabled = false` ile reliability katmanı tamamen devre dışı bırakılabilir (hızlı path korunur)
- Local-first felsefe korunur

---

## Rol Dağılımı

| Katman | Rol |
|--------|-----|
| Cloud / Claude | Planning, orchestration, review, escalation hedefi |
| Codi / Local model | Implementation, file changes, execution |
| `reliability.rs` | Decomposition kararı, verification, retry/escalation zinciri |
| `engine.rs` | Ham execution primitive (değişmez) |

Cloud, planning tarafında değil; yalnızca escalation hedefi olarak devreye girer. Decomposition kararı kural bazlı üretilir — model çağrısı yoktur.

---

## Mimari

```
codi run "görev"
       │
       ▼
  main.rs / mcp.rs
       │  run_reliable_session() çağrısı
       ▼
  reliability.rs                      ← YENİ MODÜL
  ┌─────────────────────────────────────────────────┐
  │  1. classify_task()                             │
  │     ├─ TaskProfile: write_intent? + complexity  │
  │     └─ ExecutionMode: SingleShot | Decomposed   │
  │                                                 │
  │  2. Decomposed ise → decompose()                │
  │     └─ kural bazlı Step listesi üretir          │
  │        (model tier eşiğine göre)                │
  │                                                 │
  │  3. Her step için execute_with_guard():          │
  │     a. engine::run_session_mcp()  ← dokunulmaz  │
  │     b. verify_step()                            │
  │        ├─ write_intent + diff boşsa → FAIL      │
  │        └─ read_intent → sadece exit code        │
  │     c. FAIL ise: retry(1) → cloud escalate      │
  │        → hâlâ FAIL ise explicit hata            │
  │                                                 │
  │  4. ReliabilityEvent log'a yaz                  │
  └─────────────────────────────────────────────────┘
       │
       ▼
  engine.rs                           ← DEĞİŞMEZ
  run_session_mcp()
```

---

## Yeni Dosyalar

- `crates/codi-core/src/reliability.rs`

---

## Değişen Dosyalar

| Dosya | Değişiklik |
|-------|------------|
| `config.rs` | `ReliabilityConfig` struct ve `Config.reliability` alanı |
| `main.rs` | `run_session` → `run_reliable_session` |
| `mcp.rs` | `run_session_mcp` → `run_reliable_session` |
| `signals.rs` | `VerificationFail`, `EscalationTriggered` sinyal türleri |
| `doctor.rs` | `CheckId::ReliabilityLog` kontrolü |
| `lib.rs` | `pub mod reliability;` |

---

## Bölüm A: Task Classification

### TaskProfile

```rust
pub struct TaskProfile {
    pub write_intent: bool,
    pub complexity: TaskComplexity,
    pub decision_reason: String,
}

pub enum TaskComplexity {
    Simple,
    Complex,
}

pub enum ExecutionMode {
    SingleShot,
    Decomposed(ExecutionPlan),
}
```

### Write-Intent Tespiti

Kural bazlı, fail-safe: her iki tarafta kelime yoksa **write** kabul edilir (sessiz başarısızlığı maskelemez).

**Write keywords:** `create`, `add`, `implement`, `fix`, `refactor`, `modify`, `update`, `write`, `generate`, `scaffold`, `build`, `set up`, `init`

**Read keywords:** `review`, `describe`, `analyze`, `explain`, `list`, `show`, `check`, `audit`, `read`

### Complexity Tespiti

Model tier'a göre eşik değişir. Model tier tespiti: model adı substring'i ile otomatik, `config.reliability.model_tier` ile override edilebilir.

| Tier | Kapsanan modeller (otomatik) | `decompose_threshold` varsayılanı |
|------|------------------------------|-----------------------------------|
| small | `7b`, `8b`, `3b`, `1b`, `2b` içeren isimler | 2 |
| medium | `14b`, `13b`, `32b` içeren isimler | 4 |
| large | diğerleri ve cloud modeller | 8 |

**Öncelik sırası:** `config.reliability.model_tier` > model adı substring analizi. `config.reliability.decompose_threshold` açıkça belirtilmişse tier bazlı varsayılanı ezer. İkisi de belirtilmişse `decompose_threshold` explicit değeri her zaman kazanır.

**Decomposition tetikleyici sinyaller** (eşiğe sayılan):
- Görevde dosya yolu geçmesi (`.rs`, `.ts`, `.py`, vb. uzantılar)
- "birden fazla klasör", "repository kur", "scaffold", "birkaç dosya" kalıpları
- Görev uzunluğu > 600 karakter (routing.rs ile tutarlı)

### ExecutionPlan (Decomposed)

```rust
pub struct ExecutionPlan {
    pub steps: Vec<TaskStep>,
    pub decision_reason: String,
}

pub struct TaskStep {
    pub description: String,
    pub expected_paths: Vec<String>,
}
```

Decomposition deterministik: dosya yollarını ve "klasör oluştur" komutlarını tespit edip her birini ayrı `TaskStep`'e dönüştürür. Model çağrısı yoktur.

---

## Bölüm B: Verification

### VerificationResult

```rust
pub enum VerificationResult {
    Pass,
    Fail(VerificationFailReason),
}

pub enum VerificationFailReason {
    NoDiff,
    MissingPaths(Vec<String>),
    NonZeroExit(i32),
}
```

`VerificationResult` iç temsilde enum, `.codi/reliability.jsonl`'a string serialize edilir. Bu sayede pattern analizi enum üzerinden yapılabilirken log okunabilir kalır.

### Write-Intent Görevler (Agresif)

1. `git diff HEAD --name-only` çalıştır
2. Diff boşsa → `Fail(NoDiff)`
3. `step.expected_paths` varsa → her birini kontrol et, eksikse → `Fail(MissingPaths(...))`

### Read-Intent Görevler (Konservatif)

Exit code kontrolü yeterli. Diff boşluğu failure sayılmaz.

---

## Bölüm C: Retry ve Escalation Zinciri

```
execute_with_guard(step, provider=Local, attempt=1)
    │
    ├── engine::run_session_mcp()
    └── verify_step()
         ├── Pass → log(Success) → devam
         └── Fail
              ├── attempt < max_retries:
              │   prompt biraz netleştirilir (önceki failure sebebi eklenir)
              │   attempt=2, tekrar execute_with_guard()
              │   verify_step()
              │        ├── Pass → log(RetrySuccess)
              │        └── Fail
              │             ├── cloud varsa + escalate_on_retry_failure=true:
              │             │   execute_with_guard(cloud_provider, attempt=3)
              │             │   verify_step()
              │             │        ├── Pass → log(EscalationSuccess)
              │             │        └── Fail → log(EscalationFail) → Err(explicit)
              │             └── cloud yoksa → log(RetryFail) → Err(explicit)
              └── max_retries=0: doğrudan escalation veya Err
```

**Retry prompt netleştirme** — önceki failure sebebi görev başına eklenir:
> "Önceki denemede dosya yazılmadı. Şimdi yalnızca şunu yap: [orijinal görev]"

Hata hiçbir zaman swallow edilmez; her failure loglanır ve `Result::Err` olarak yüzeye çıkar.

---

## Bölüm D: ReliabilityEvent ve Log

### ReliabilityEvent

```rust
pub struct ReliabilityEvent {
    pub task_id: String,
    pub task_snippet: String,          // ilk 120 karakter
    pub step_index: usize,
    pub execution_mode: String,        // "single_shot" | "decomposed"
    pub provider: String,              // "local(qwen2.5:7b)" | "cloud(claude-...)"
    pub attempt: u8,                   // 1=ilk, 2=retry, 3=escalation
    pub exit_code: i32,
    pub verification: VerificationResult, // enum, string serialize
    pub outcome: String,               // "success" | "retry_success" | "escalation_success" | "fail"
    pub decision_reason: String,
    pub timestamp: u64,
}
```

**Log dosyası:** `.codi/reliability.jsonl` — her event bir satır JSON olarak append edilir.

**Güvenlik:** `log_path` yalnızca relative path kabul eder. Yükleme sırasında canonicalize + repo root dışına çıkış engellenir. Path traversal (`../`, absolute path) hata döndürür.

---

## Bölüm E: Config

### `ReliabilityConfig`

```toml
[reliability]
enabled = true
decompose_threshold = 2      # model tier varsa bu override edilir
model_tier = ""              # "small" | "medium" | "large" — boşsa isimden çıkarılır
verify_artifacts = true
max_retries = 1
escalate_on_retry_failure = true
log_events = true
log_path = ".codi/reliability.jsonl"
```

`enabled = false` durumunda `run_reliable_session()` doğrudan `engine::run_session_mcp()` çağırır — sıfır ek overhead.

---

## Bölüm F: Doctor Entegrasyonu

### Yeni `CheckId::ReliabilityLog`

`.codi/reliability.jsonl` okunur, son 20 event analiz edilir:

- Başarı oranı hesaplanır
- `NoDiff` pattern'i sayılır (sessiz başarısızlık tespiti)
- Escalation sayısı raporlanır

**Örnek çıktı:**

```
[⚠] reliability         %67 success (4/6), 2 silent failures, 1 escalation
    → codi doctor --fix yapamaz; codi reliability komutu ile detay görün
[✓] reliability         100% success (8/8) — son 20 event
```

Log dosyası yoksa (reliability ilk kez etkinleştirildi veya henüz run yok): `Info` severity ile raporlanır.

---

## Bölüm G: Signals Entegrasyonu

### Yeni SignalKind türleri

```rust
SignalKind::VerificationFail {
    task_snippet: String,
    missing_paths: Vec<String>,
    reason: String,       // "no_diff" | "missing_paths" | "nonzero_exit"
},
SignalKind::EscalationTriggered {
    reason: String,
    escalation_provider: String,
},
```

Bu sinyaller mevcut `post_run_hook()` akışına dahil edilerek `self_improvement` kandidatları üretmeye devam eder.

---

## Public API

```rust
pub fn run_reliable_session(
    cfg: &Config,
    task: &str,
    repo_root: &Path,
    ctx: RunContext,
) -> Result<ReliabilityOutcome>

pub enum RunContext { Cli, Mcp }

pub struct ReliabilityOutcome {
    pub success: bool,
    pub exit_code: i32,
    pub execution_mode: ExecutionMode,
    pub steps_total: usize,
    pub steps_succeeded: usize,
    pub decision_reason: String,
    pub events: Vec<ReliabilityEvent>,
}
```

---

## Test Stratejisi

- `classify_task()` için unit testler: write-intent tespiti, tier bazlı eşik, decomposition tetikleyiciler
- `verify_step()` için unit testler: boş diff = fail, expected path eksikse fail, read-intent = no fail
- `execute_with_guard()` için integration testler: retry zinciri, escalation (mock engine ile)
- `ReliabilityEvent` serialize/deserialize round-trip
- `doctor` check: log dosyası yoksa Info, başarı oranı düşükse Warning

---

## Açık Kalan Kararlar

Yok — tüm tasarım bölümleri onaylandı.
