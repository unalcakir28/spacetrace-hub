# Sürüm

Bu depo private (K2), ama ikililerin indirme bağlantısı public olmak zorunda —
private bir deponun release varlıkları kimlik doğrulaması olmadan indirilemiyor.
Bu yüzden `.github/workflows/release.yml` burada derliyor, **public
`unalcakir28/spacetrace` deposuna yayınlıyor**. Konteyner imajı ise doğrudan
GHCR'a gidiyor.

Tam gerekçe ve üç bileşenin ortak şeması: çekirdek deposundaki
[docs/RELEASING.md](https://github.com/unalcakir28/spacetrace/blob/main/docs/RELEASING.md).

## Ne zaman ne oluyor

| Olay | İkililer | İmaj |
|------|----------|------|
| `main`'e push | public depoda `hub-continuous` | `ghcr.io/…/spacetrace-hub:main`, `:edge` |
| `v*` etiketi push | public depoda `hub-v*` | `:v*`, `:latest` |
| `workflow_dispatch` | derler, yayınlamaz — `publish: true` verilmedikçe | — |

`*.md` ve `tasks/**` değişiklikleri iş akışını tetiklemiyor.

Üretilen arşivler — adları sabit, indirme sayfası bunlara doğrudan bağlanıyor:

```
spacetrace-hub-<sürüm>-x86_64-unknown-linux-musl.tar.gz
spacetrace-hub-<sürüm>-aarch64-unknown-linux-musl.tar.gz
spacetrace-hub-<sürüm>-aarch64-apple-darwin.tar.gz
spacetrace-hub-<sürüm>-x86_64-apple-darwin.tar.gz
SHA256SUMS
```

Her arşivde ikili, README ve `docker-compose.yml` var.

## İmaj neden kaynaktan derlenmiyor

`.github/docker/Dockerfile.release` yalnızca **önceden derlenmiş** musl ikilisini
kopyalıyor; buildx her platform için `TARGETARCH`'ı ayarlıyor. Depo kökündeki
`Dockerfile` kaynaktan derlemeye devam ediyor ve `docker build .` bir klonda
çalışıyor — ama çok mimarili bir sürüm derlemesi için yanlış araç: amd64 bir
runner'da arm64 imajını böyle kurmak Rust'ı QEMU altında derlemek demek, onlarca
dakika sürüyor ve düzenli olarak belleği tüketiyor. İkililer zaten `cross` ile
doğal hızda derleniyor, dolayısıyla ikinci kez derlemenin bir anlamı yok.

## Gereken elle adımlar

### 1. `RELEASE_TOKEN`

```bash
gh secret set RELEASE_TOKEN --repo unalcakir28/spacetrace-hub
```

Fine-grained PAT, yalnızca `unalcakir28/spacetrace` deposunda **Contents: Read
and write**. Sır yoksa iş akışı hata vermez — varlıkları bu private depoda
yayınlar ve uyarı basar; imaj yayını etkilenmez.

### 2. İmajı public yap

Private bir depodan oluşturulan GHCR paketi private başlıyor, yani
`docker pull` kimlik doğrulaması ister. Bir kez:

<https://github.com/users/unalcakir28/packages/container/spacetrace-hub/settings>
→ Change visibility → **Public**.

## Kararlı sürüm kesmek

`Cargo.toml` içindeki `version`'ı yükselt, sonra:

```bash
git tag v0.2.0 && git push origin v0.2.0
```
