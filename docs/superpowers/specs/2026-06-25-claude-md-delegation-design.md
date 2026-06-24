# CLAUDE.md Delegasyon Talimatı — Tasarım

## Hedef

`codi init` yalnızca MCP araçlarını görünür kılıyor; Claude'a bu araçları ne zaman kullanacağını söylemiyor. Eksik parça davranışsal yönlendirme: proje kökünde bir `CLAUDE.md` oluşturarak Claude'a implementasyon işlerini codi'ye devretmesini açıkça söylemek.

## Kapsam

**Kapsam içi:**
- `codi init` yeni adım [6/6]: CLAUDE.md oluştur veya merge et
- `codi doctor` yeni check: `ClaudeMd` — bölüm var mı?
- `codi doctor --fix` yeni fix: eksik bölümü dosyanın sonuna ekle

**Kapsam dışı:**
- MCP tool description'larını daha yönlendirici yazmak (ayrı follow-up)

## CLAUDE.md İçeriği

`codi init` tarafından yazılan veya merge edilen bölüm:

```markdown
## codi — AI coding agent

Bu projede mümkün olduğunda doğrudan kod yazmak yerine codi MCP araçlarını
tercih et:

- **Kod yaz / refactor / bug fix** → `run_task` ile codi'ye devret
- **Değişiklikleri incele** → `get_diff` ile diff al, sen yorumla
- **Testleri doğrula** → `run_tests` ile çalıştır

Claude'un rolü: planlama, review, güvenlik, koordinasyon.
codi'nin rolü: implementasyon (yerel LLM üzerinden Goose ile çalışır).
```

Bölüm başlığı `## codi` — varlığı kontrol için marker olarak kullanılır.

## Mimari

### `codi-core/src/init.rs`

`run_init` içine yeni adım:

```
[6/6] CLAUDE.md
```

Yeni fonksiyon `ensure_claude_md(repo_root: &Path) -> Result<()>`:

| Durum | Davranış | Çıktı |
|-------|----------|-------|
| CLAUDE.md yok | Dosyayı oluştur, bölümü yaz | `✓ CLAUDE.md oluşturuldu` |
| CLAUDE.md var, `## codi` yok | Dosyanın sonuna `\n\n## codi …` ekle | `✓ CLAUDE.md — codi bölümü eklendi` |
| CLAUDE.md var, `## codi` var | Dokunma | `✓ CLAUDE.md — değiştirilmedi` |

Mevcut CLAUDE.md içeriği asla silinmez veya değiştirilmez.

### `codi-core/src/doctor.rs`

Yeni `CheckId::ClaudeMd` değeri.

`run_doctor` içine yeni check (check 7 — mevcut 6 check'in ardından):

| Koşul | Severity | Fixable |
|-------|----------|---------|
| CLAUDE.md yok | `Error` | `true` |
| CLAUDE.md var, `## codi` bölümü yok | `Error` | `true` |
| CLAUDE.md var, `## codi` bölümü var | `Ok` | `false` |

`run_doctor_fix` içine `CheckId::ClaudeMd` arm: `ensure_claude_md(repo_root)` çağır.

## Adım Numaralandırması

Mevcut `[1/5]`…`[5/5]` → `[1/6]`…`[6/6]` olarak güncellenir.

## Test Senaryoları

- `ensure_claude_md` — CLAUDE.md yokken oluşturur, `## codi` içerir
- `ensure_claude_md` — `## codi` olmayan mevcut dosyaya ekler, orijinal içeriği korur
- `ensure_claude_md` — `## codi` olan dosyaya dokunmaz
- `doctor` — CLAUDE.md yoksa `ClaudeMd` check `Error` döner
- `doctor` — `## codi` olmayan CLAUDE.md varsa `Error` döner
- `doctor` — geçerli CLAUDE.md varsa `Ok` döner
- `doctor --fix` — CLAUDE.md yoksa oluşturur, severity `Ok` olur
