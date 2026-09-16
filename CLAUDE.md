# spacetrace hub — Claude için proje notları

Filo panosu: ajanların push ettiği snapshot'ları alır, `(host, root)` hedefleri
hâlinde gösterir, trend çıkarır, uyarı gönderir. axum + SQLite, **build step
yok** — sunucu HTML'i doğrudan üretiyor. Tarayıcı, ağaç modeli ve snapshot
deposu burada değil: [unalcakir28/spacetrace](https://github.com/unalcakir28/spacetrace)
çekirdek deposunda, git bağımlılığı olarak alınıyor.

Zaten iyi anlatılmış olanı tekrarlamıyorum: [README.md](README.md) kurulumu,
iki kimlik bilgisi tablosunu, **tahminin dürüstlüğü** kurallarını, `src/`
düzenini ve lisansı tutuyor; [RELEASING.md](RELEASING.md) kanal tablosunu,
sabit arşiv adlarını, imajın niye kaynaktan derlenmediğini ve iki elle
yapılacak kurulum adımını tutuyor. Yol haritası ve fazlar arası kararlar
çekirdek deponun `TODO.md` ve `docs/DECISIONS.md` dosyalarında (K1 İngilizce,
K2 ticari lisans).

Bu dosya yalnızca **ikisinde de yazmayan, sessizce bozulabilen** şeyleri
anlatıyor.

## Komutlar

```bash
cargo build --all-targets                    # CI böyle derliyor
cargo test                                   # servis konteyneri gerekmez
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

spacetrace-hub --config hub.toml init        # başlangıç config'ini bas
spacetrace-hub --config hub.toml check       # config + db doğrula, filo özeti
spacetrace-hub --config hub.toml token nas   # ajan tokeni (bir kez yazılır)
spacetrace-hub --config hub.toml serve
```

`--config` global, varsayılanı `/etc/spacetrace-hub/hub.toml`. `init` config
yokken koşuyor; diğer her alt komut config'i yükleyip **iki migration'ı da**
çalıştırıyor. Rust 1.85+, ikili adı `spacetrace-hub`, kütüphane adı
`spacetrace_hub` — kütüphane olarak açılması HTTP yüzeyinin uçtan uca test
edilebilmesi için.

Çapraz derleme dört hedef: `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl` (ikisi `cross` ile), `aarch64-apple-darwin`,
`x86_64-apple-darwin` (ikisi native).

**`Cross.toml` şart.** Sürüm damgası (`SPACETRACE_GIT_SHA`,
`SPACETRACE_BUILD_DATE`, `SPACETRACE_CHANNEL`) konteynere ancak oradaki
passthrough listesiyle giriyor; liste eksikse musl ikilileri **damgasız**
derleniyor.

`scripts/`, Makefile, `.claude/` ve `tasks/` yok. `.playwright-mcp/`
gitignore'da — sayfaları görsel olarak gözden geçirirken biriken
tarayıcı-otomasyonu önbelleği (20 ekran görüntüsü, 31 erişilebilirlik
snapshot'ı, bir konsol logu), otorite değil.

## Çekirdek bağımlılığı: pin yalnızca Cargo.lock'ta

`Cargo.toml` beş çekirdek crate'i (`scan-core`, `store`, `diff`, `changelog`,
`buildinfo`) **rev, branch ya da tag olmadan** git'ten alıyor. Path override ve
`[patch]` yok — yanındaki `../spacetrace` checkout'u **kullanılmıyor**.

Yani tek pin `Cargo.lock`. Sonuçları:

- `cargo update` **uzak deponun `main`'ini** çözüyor. Yanındaki
  `../spacetrace` checkout'u bu işe hiç karışmıyor — yereldeki commit'ler
  görünmüyor, riski **push edilmiş** olanlar yaratıyor: uzak `main` kilitli
  rev'in ilerisindeyse `cargo update` incelenmemiş çekirdek değişikliğini
  sessizce içeri alır. Pin ilerletmek bilinçli bir iş.
- **CI `--locked` geçmiyor** (`ci.yml`), yalnızca `release.yml` geçiyor ve
  gerekçesi orada yazılı: onsuz **tek bir etiketin iki derlemesi farklı
  tarayıcı kodu gömebilir**.
- Pin'i ilerletip `CHANGELOG.md`'yi üretmezsen `tests/changelog.rs` kırılıyor
  — **bu test aynı zamanda pin muhafızı**, testin yorumu da bunu söylüyor.
- Çekirdeğin genel API'sini bozan değişikliği bu deponun CI'ı görmüyor;
  oradaki `downstream-api-guard` ajanı tam bunun için var.

## Bozulmaması gereken şeyler

1. **Router grubu yetkilendirmenin kendisi.** Yazma route'ları ayrı bir grupta
   (`require_write`), okuma route'ları ayrı, `/snapshots` yalnızca ajan
   tokeniyle, `/health` ve `/login` kimliksiz. Her handler'ın içine bir
   kontrol koymak unutulacak bir şeydir, ve burada unutmak **filodaki her
   uyarı kuralını silebilen bir salt-okunur hesap** demek.

   Muhafızı `tests/api.rs` içindeki `every_write_route_is_in_this_list`:
   `src/web.rs`'i **metin olarak parse ediyor**, her `.route(…, post(…))`'u
   bulup `WRITE_ROUTES`'ta yoksa düşüyor. Beyaz listede yalnızca `/login`,
   `/logout`, `/snapshots` var. **`WRITE_ROUTES`'u güncellemeden POST route
   eklemek test takımını kırar.**
2. **İki kimlik bilgisi birbirinin yerine geçmez.** Ajan tokeni yalnızca
   `POST /snapshots` yapabiliyor; kişi hesabı (`viewer` | `admin`) panoyu
   görüyor. Config'teki admin tokeni **ilk** kontrol ediliyor — sorgu maliyeti
   olmasın ve kısa süre erişilemez bir veritabanı geri dönüş yolunu
   kilitlemesin diye.
3. **Tokenler düz SHA-256 olarak saklanıyor, KDF yok** ve bu bilinçli: 256
   bitlik makine rastgeleliğinde brute-force edilecek bir şey yok. Yeni
   oluşturulan token yönlendirme sorgu dizesinde taşınıyor — düz metni tam bir
   kez gösterilsin ve veritabanına hiç yazılmasın diye.
4. **Son aktif admin iptal edilemez.**
5. **Ayarlar sayfası tasarımı gereği salt-okunur.** SMTP bilgilerini geri
   yazan bir form sırrı veritabanına koyardı — insanların hub'ı taşırken
   kopyaladığı ve hata raporuna eklediği dosya o. Kullanıcı adı da,
   parolanın uzunluğu da gösterilmiyor.
6. **`/settings/test` bilerek dry-run değil**: açıyor, kimlik doğruluyor ve
   gerçek bir posta gönderiyor — üçü de mail yapılandırmasının yanlış olma
   yollarından biri.
7. **Uyarı olayını kaydetmek cooldown'u başlatan şeydir.** `COOLDOWN_SECONDS`
   6 saat, `(kural, host, root)` başına; `collect()` hem karar veriyor hem
   kaydediyor, gönderim ayrı çünkü yavaş ve başarısız olabiliyor.
   `record_event` `fired_at`'i parametre alıyor ki cooldown kaydedildiği
   saatle karşılaştırılsın.
8. **Gönderim hatası kaydedilir, asla yeniden denenmez.** Gitmiş bir
   endpoint için kuyruk tutan bir hub kendi başına bir probleme dönüşüyor;
   yeniden denemeyi doğru yapan şey relay'in kendisi.
9. **Tahmin sert tutuluyor**: en az 3 örnek, 1 günlük açıklık, `R² ≥ 0.5`,
   kapasite ölçülmüş olmalı, ufuk ≤ 3650 gün. "Kendinden emin yanlış bir
   tarih, tarih olmamasından kötüdür." Ve **büyüme taranan klasörde ölçülüyor,
   tahmin dosya sisteminin boş alanını projekte ediyor** — aynı sayı değil.
10. **İleri uyumlu okuma**: tanınmayan bir `alert_rules.kind`
    `FreeBelowPercent`'e, ayrıştırılamayan bir `destination` webhook'a
    düşüyor — tüm kural listesini okunamaz kılmak yerine.

## Ajan ↔ hub protokolü

- Gövde **ham SQLite snapshot dosyası** — `spacetrace-agent push`'un
  gönderdiğiyle ve `GET /scans/{id}/download`'un sunduğuyla bayt bayt aynı.
  Ajanın hub'a özel bir kipi yok, hub'ın kendi formatı yok.
- Yol: gövde → (varsa) sınırlı zstd çözme → `tempdir()`'e yaz →
  `Store::import_snapshot` → retention budama, hepsi `spawn_blocking` içinde.
- **zstd çözme `max_upload_bytes` ile sınırlı.** Body limiti yalnızca
  *sıkıştırılmış* boyutu sınırlıyor; birkaç kilobayt hub'ı tüketebilirdi.
  Bomba testi var.
- **Yinelenme kontrolü çekirdek store'da, burada değil.** Gelen bir tarama
  `(host, root, started_at)` üçlüsüyle zaten varsa atlanıyor — yeniden push
  no-op ve `imported: []` dönüyor.
- **`import_snapshot` bir güven sınırı** ve ağı geçmiş bir gövdenin açıldığı
  tek yer. Her taramanın `content_hash`'i yeniden hesaplanıp karşılaştırılıyor;
  uyuşmazlık **tüm import'u** düşürüyor ("bozuk olanı atla" değil — işlem
  ya hep ya hiç ve bir push tam bir tarama taşıyor). İstemciye **400** olarak
  dönüyor, çünkü snapshot olmayan bir gövde gönderenin hatası. Neden önemli:
  dönmüş bir bit yapı olarak kusursuz bir ağaç bırakıp yanlış rakam taşıyor,
  ve aciliyete göre sıralı bir panoda **yanlış makineyi tepeye** koyuyor.
- Tarama id'leri import'ta yeniden atanıyor; `entries.id` yeniden
  yazılmıyor çünkü o taramanın kendi arenasına indeks ve
  `children_start`/`children_len` ile eşleşmek zorunda.

## Yok olan şeyler (ve bunu bilerek yazıyorum)

- **Hız sınırlama yok. `X-Forwarded-For` / `X-Real-IP` / `ConnectInfo`
  işlenmiyor.** Hub istemci IP'sini hiç görmüyor ve loglamıyor. Çekirdek
  CLAUDE.md'de anlatılan token bucket **ajanda**, burada değil — ikisini
  karıştırma. Tek gövde savunması `/snapshots` üzerindeki
  `DefaultBodyLimit::max(max_upload_bytes)`.
- **TLS yok.** Ters vekil bekleniyor ve dört dosyada yazılı (README,
  `docker-compose.yml`, `config.rs`, `web.rs`). Sonucu: oturum
  çerezinde `Secure` bayrağı **yok** — onu önüne TLS koyarak ekliyorsun.
- **Background job, zamanlayıcı, spawn edilmiş worker yok.** İki türetilmiş iş
  de import isteğinin içinde senkron koşuyor: retention `import`'un içinde
  ("veritabanını büyüten tek şey import, sınırlanması gereken an tam bu") ve
  uyarı değerlendirmesi hemen ardından, aynı istekte.
- **Bağlantı havuzu yok** — istek başına açılıyor (`foreign_keys=ON`,
  `busy_timeout=30s`).
- **Frontend build step yok.** HTML `src/web.rs` içinde string birleştirmeyle
  kuruluyor, stylesheet `src/html.rs` içinde `const STYLE` olarak gömülü.
  Template crate'i, gömülü varlık, JS bundle yok. Tek grafik elle yazılmış
  inline SVG polyline — chart kütüphanesi asacak bir build adımı olmadığı
  için. Gerekçe README'de: "sana bir sayfa göstermeden önce `npm install`
  isteyen bir self-hosted araç, daha kötü bir araçtır."
- **Rollup tablosu yok**: filo özeti ve aciliyet sıralaması saklanan
  snapshot'lardan türetiliyor.

## Yapılandırma

Tek TOML dosyası, `--config` ile. **Her iki struct'ta
`deny_unknown_fields`** — yazım hatası olan bir anahtar sert hata, sessizce
yoksayılmıyor.

`db` zorunlu. `listen` varsayılanı `0.0.0.0:8080` (bilerek tüm arayüzler).
`max_upload_bytes` 512 MiB. `keep_per_target` **ayarlanmazsa sınırsız** —
veritabanı sınırsız büyür. `[smtp]` bloğu yoksa hub posta göndermiyor
(webhook kuralları etkilenmiyor); `smtp.password_file` inline `password`'ü
eziyor, `security = "none"` + kullanıcı adı reddediliyor (parola düz metin
giderdi).

Admin tokeni sırası: inline `admin_token` → `admin_token_file` → ortam
değişkeni `SPACETRACE_HUB_ADMIN_TOKEN`. **Boş bir token dosyası hata, boş
token değil**; boş inline token ise sonraki kaynağa düşüyor. `serve` admin
tokeni olmadan **başlamayı reddediyor**.

Sunucunun okuduğu ortam değişkeni bundan başka **yok** (`envy` yok, `config`
crate'i yok, `.env` işleme yok). Diğer üçü derleme zamanı damgası.

## Şema: iki migration, iki sürüm

Tek SQLite dosyası hem çekirdek snapshot store'unu hem hub'ın kendi
tablolarını tutuyor, ve ikisinin migration'ı bağımsız:

- `Store::open` store'un migration'ını koşuyor ve daha yeni bir şemayı
  reddediyor.
- `db::migrate` `hub_meta`, `agent_tokens`, `dashboard_users`, `alert_rules`,
  `alert_events` kuruyor; `HUB_SCHEMA_VERSION = 3`, `hub_meta.schema`'da, daha
  yeni bir değer sert hata. **Sürüm adımları `CREATE TABLE IF NOT EXISTS`
  toplu işinden önce koşuyor** ve bu bilinçli.
- `Destination` **tek sütunda URI**, nullable bir çift değil — "tam olarak bir
  hedef" verinin şekli olsun diye. E-posta eklenirken `mailto:` şeması yeniden
  kullanıldı ki mevcut webhook satırları dönüştürülmesin. Adresler form
  anında `lettre::message::Mailbox` ile doğrulanıyor — yazım hatası
  düzeltebilecek kişinin önünde reddedilsin, gece 3'te patlamasın diye.
- `alert_rules.created_by` nullable ve NULL "kimsenin adı yokken" demek;
  backfill yapılmadı, çünkü uydurmak **yapmadığı bir şeye birinin adını
  yazmak** olurdu. Em dash olarak render ediliyor, boş bırakılmıyor — boş
  hücre render hatası gibi okunuyor.

## Alışkanlıklar

- **Arayüz, kod ve yorumlar İngilizce** (çekirdek K1): `<html lang="en">`
  sabit, About kartı beş dilli changelog girdisinin İngilizcesini basıyor.
  Hub bir operatör aracı ve araç İngilizce; aynı girdileri çeviren yüzeyler
  site ve masaüstü. **Türkçe kalan: commit mesajları, `RELEASING.md` ve bu
  dosya.**
- **El yazısı tercih ediliyor**: `constant_time_eq`, `cookie_value`,
  `urlencode` elle yazıldı. **`lettre` bilinçli istisna** — ağdan gelmiş bir
  hostname ya da yoldaki çıplak CRLF header bloğunu bitirip gerisini header
  olarak okutabilir (enjekte edilmiş bir `Bcc:`); lettre bunu tip seviyesinde
  reddediyor. rustls-only, statik musl ikilisi hayatta kalsın diye.
- `CHANGELOG.md` **üretiliyor, elle düzenlenmiyor**: çekirdek checkout'undan
  `cargo run -p spacetrace-changelog -- markdown --component hub > CHANGELOG.md`.
- About kartı changelog'u **ikiliye derleniyor, indirilmiyor** — hava boşluklu
  bir makinede de aynı okunsun diye; en yeni 5 sürüm. **`unreleased`
  gösterilmiyor**, ve muhafızı `<h3>` sayıyor, "unreleased" metnini
  aramıyor: her sürümden hemen sonra `unreleased` boş olur, yani metin arayan
  bir test tam regresyonun geçeceği anda yeşil kalır.
- `yarn`/npm yok, bağımlılık eklemekte cimri ol — bkz. yukarıdaki "build step
  yok".
- **Çekirdek deponun `.claude/` araçları burada geçerli değil.** Changelog
  girdisi yazmak hâlâ çekirdek checkout'undan sürülüyor: kaynak
  `crates/changelog/changelog.json` orada.

## Depoda duran Claude araçları

| Araç | Ne zaman |
|------|----------|
| `preflight` (beceri) | Push öncesi; `--locked` farkı ve iki yanıltıcı test hatası orada yazılı |
| `release` (beceri) | Sürüm kesme; sürüm notlarının niye awk ile dilimlendiği dahil |

İkisi de `disable-model-invocation`: kullanıcı `/preflight`, `/release` yazar.

Paylaşılan araçlar `spacetrace-tools` plugin'inden geliyor ve `spacetrace-tools:`
ile adlandırılıyor: `core-pin-guard` (pin ilerletmeden önce çekirdek API
diff'i), `doc-drift-auditor`, `code-reviewer`, `test-writer`, ve üretilen
`CHANGELOG.md`'yi koruyan hook — bu depoya ayrı bir hook gerekmedi. Plugin
**depoda değil**, ana dizindeki `spacetrace-tooling/` içinde — klonla
gelmiyor.

## Testler

```bash
cargo test    # servis konteyneri, canlı DB ya da ağ erişimi gerekmez
```

- `tests/api.rs` — asıl kanıtlanmaya değer şey **iki kimlik bilgisi
  arasındaki ayrım**. Harness inline TOML'den `Config` kurup **iki
  migration'ı da `main.rs` gibi** koşuyor, sonra `127.0.0.1:0`'da gerçek bir
  socket açıyor. Her şey `tempdir()` içinde.
- Test gövdeleri **gerçek snapshot**: `spacetrace_scan_core::scan` → `save` →
  `export_snapshot`. Tekrarlı push'lar **çağıranın sahip olduğu** bir dizini
  taramak zorunda — her seferinde yeni geçici yol, her push'ta yeni makine
  gibi görünür.
- Webhook gönderimi yerel sockette **gerçek alıcı** ayağa kaldırılarak
  gözlemleniyor. SMTP testleri gönderimi değil, sayfayı ve kural kurulumunu
  doğruluyor.
- Soğuk derleme **ağ** (git bağımlılıkları) ve **çalışan bir C toolchain**
  istiyor: `rusqlite` `bundled` özelliğiyle SQLite'ı kaynaktan derliyor,
  `zstd` ve lettre'in `ring`'i de C/asm taşıyor. Kökteki Dockerfile'ın
  `apk add musl-dev` yapması bu yüzden.
- CI matrisi `ubuntu-latest` + `macos-latest`, toolchain `stable` (1.85 değil).

## Sürüm

Tam sıra [RELEASING.md](RELEASING.md); kolay bozulan kısımlar:

- **İki Dockerfile var ve kökteki release yolu DEĞİL.** Kökteki
  (`rust:1.85-alpine`, kaynaktan derliyor) yalnızca `docker build .` bir
  klonda çalışsın diye duruyor. Gerçek yol `.github/docker/Dockerfile.release`:
  `alpine:3.21` + `COPY bin/${TARGETARCH}/spacetrace-hub`, **hiçbir şey
  derlemiyor**. Gerekçe dosyanın içinde — amd64 runner'da QEMU altında Rust
  derlemek on dakikalarca sürüyor ve düzenli olarak bellek tüketiyor.
- Varlık **dizin olarak** yükleniyor (`image`), glob (`image/**`) olarak
  değil: glob'la artifact kökü `image/bin/<arch>` oluyor ve Dockerfile'ın
  kopyaladığı düzen kayboluyor.
- **Etiket/manifest muhafızı**: gerçek bir tag push'unda ad ≠ `v` +
  `Cargo.toml` sürümü ise `meta` job düşüyor — kendi sürümü hakkında yalan
  söyleyen bir ikili yayınlanmasın diye. Dispatch etiketleri muaf.
- **İkililer çekirdek depoya yayınlanıyor** (`RELEASE_TOKEN`); sır yoksa iş
  akışı **düşmüyor**, bu özel depoya yayınlayıp indirme sayfasının onları
  alamayacağını söyleyen bir uyarı basıyor. Etiketler: `hub-continuous`
  (her seferinde silinip yeniden kuruluyor) ve `hub-v*`.
- **Kararlı sürümlerde `--latest=false`.** Çekirdeğin sürüm listesi üç
  bileşeni tutuyor; o endpoint CLI'ya ait ve `spacetrace update` bir hub
  etiketini hiç sürüm değilmiş gibi ayrıştırıyor — o kurulumlar kalıcı olarak
  sessizleşiyor (9 Eylül 2026'da hub-v0.3.0 yeri aldı).
- **Sürüm notları `CHANGELOG.md`'den `awk` ile dilimleniyor, üretilmiyor.**
  Sebep: `CHANGELOG.md` *pinlenmiş* changelog crate'inden üretiliyor ve bir
  test güncel olduğunu iddia ediyor — dilimlemek, notların ikiliye derlenmiş
  changelog'la aynı olmasını garanti ediyor. Üreteci çekirdeğin `main`'ine
  karşı koşturmak bunu garanti etmezdi. Etiketli bir sürümde bölüm boşsa sert
  hata.
- `paths-ignore` README/RELEASING/tasks'ı dışlıyor ama **`CHANGELOG.md`'yi
  bilerek dışlamıyor** — o sürüm notunun kendisi. Filtre tag push'larına da
  uyguluyor; kaçış yolu `publish: true` ile elle `workflow_dispatch`.
- Checksum adımı `find . -maxdepth 1 -type f` kullanıyor, çünkü
  `sha256sum ./*` ilk dizinde ölüyor — bir varlığın içindeki başıboş bir
  klasör tüm sürümü böyle düşürüyor.
- İmajlar: rolling → `:main` + `:edge`, etiketli → `:v<x>` + `:latest`.

## Sayılar bayatlıyor

README'deki HTTP API tablosu bir kez sessizce bayatladı — `/people`,
`/settings`, `/about` ve bütün POST route'ları eklendikten sonra tablo
öncesinden kalmıştı ve okuma grubunu "admin" diye etiketliyordu, oysa kod
oturum açmış her role izin veriyor. Test sayısı da öyle: "94 tests" yazarken
gerçek 123'tü. **Route eklerken tabloyu, test ekleyince sayıyı aynı commit'te
güncelle** — ikisi de kimsenin bakmadığı yerde yanlışa dönüşüyor.
