# انتشار ClipNest

از صفر تا نسخه‌ای که کاربر می‌تواند نصب کند. هر بلوک را می‌توانی کپی کنی؛
جاهایی که باید اسم خودت را بگذاری با `<...>` مشخص شده.

## ۰) پیش از اولین push

```bash
# ۱. نام حساب گیت‌هاب را همه‌جا بگذار (Cargo.toml، README، اسپک RPM، PKGBUILD)
make set-github GH=<account>

# ۲. پروژه سالم است؟
make check                  # clippy با -D warnings + ۶۳ تست

# ۳. چه چیزی کم است؟ (فقط می‌خواند و دستور بعدی را چاپ می‌کند)
make preflight
```

`make preflight` همان چیزهایی را می‌گیرد که از داخل کد دیده نمی‌شوند و موقع
انتشار شبیه «پروژه خراب است» به نظر می‌رسند، در حالی که فقط یک قدم جا افتاده:
نبودِ هویت git (git اصلاً کامیت نمی‌کند)، باقی‌ماندن `USERNAME` در متادیتا
(نسخه‌ای که به مخزن ناموجود اشاره می‌کند)، و `dist/`ی که پیش از آخرین ویرایش
ساخته شده (سورس تازه تگ می‌خورد ولی باینری قدیمی منتشر می‌شود). در پایان هم
دستورهای واقعی push را با نسخهٔ درست چاپ می‌کند.

### هویت git و احراز هویت — تنها چیزی که خودِ پروژه نمی‌تواند بسازد

اگر روی این ماشین قبلاً گیت استفاده نشده باشد، **اولین کامیت شکست می‌خورد**
(`Author identity unknown`) و **push هم نام کاربری و توکن می‌خواهد**. یک‌بار برای
همیشه (برای هر پروژه):

```bash
git config --global user.name  'Iman Elyasi'
git config --global user.email 'imaneliasy549@gmail.com'   # همان ایمیل حساب گیت‌هاب

# و یکی از این دو راه برای push:
# الف) توکن روی HTTPS — ساده‌ترین راه، بدون کلید SSH
#     github.com/settings/tokens → Fine-grained token → فقط همین مخزن →
#     Contents: Read and write. بعد موقع push، نام کاربری = حساب گیت‌هاب،
#     رمز = همان توکن (نه پسورد حساب).
# ب) کلید SSH
ssh-keygen -t ed25519 -C 'imaneliasy549@gmail.com'   # Enter, Enter, Enter
cat ~/.ssh/id_ed25519.pub                            # این را در
#     github.com/settings/keys → New SSH key بگذار، بعد از آدرس SSH استفاده کن:
#     git remote add origin git@github.com:<account>/clipnest.git
```

> توکن را داخل آدرس remote نگذار (`https://user:ghp_...@github.com/...`)؛ چون
> در `.git/config` ذخیره می‌شود و ماندگار است. اگر می‌خواهی گیت خودش یادش بماند،
> `git config --global credential.helper store` را بعد از اولین push موفق بزن.

`make set-github` این چهار چیز را عوض می‌کند: دو خط کامنت‌شدهٔ `homepage` و
`repository` در `Cargo.toml` (که فیلد Homepage بستهٔ `.deb` را پر می‌کنند)، آدرس‌های
`github.com/imaneliasy549-oss` در `README.md` و `RELEASING.md`، `url` در `PKGBUILD` و `URL`
در اسپک RPM. اگر این کار را نکنی، `make dist` هم یادآوری می‌کند و بسته‌ای با آدرس
ناموجود منتشر نمی‌شود.

`Cargo.lock` **باید** در مخزن باشد (پروژه یک باینری است، نه کتابخانه)، پس آن را
از `.gitignore` بیرون بگذار اگر جایی اضافه‌اش کردی. دیتابیس تاریخچه و فایل تنظیمات
بیرون از پروژه‌اند (`~/.local/share/clipnest`، `~/.config/clipnest`) و هیچ‌وقت
داخل مخزن نیستند.

## ۱) اولین push

**۱. مخزن را در گیت‌هاب بساز.** در <https://github.com/new>:

- Repository name: `clipnest`
- Visibility: Public (برای این‌که رانرهای arm64 رایگان و Actions بی‌محدودیت باشند)
- **Add a README file، Add .gitignore و Choose a license را تیک نزن.** اگر تیک بزنی،
  مخزن از قبل یک کامیت دارد و push اولت رد می‌شود (`fetch first`) و مجبور می‌شوی
  merge کنی.
- Create repository. صفحه‌ای می‌آید که آدرس مخزن و همان دستورهای زیر را نشان می‌دهد.

**۲. کامیت اول و push.** این‌ها را خودت اجرا کن — من عمداً commit یا push نمی‌کنم:

```bash
git init -b main
git add .
git status --short          # یک نگاه: نباید target/ یا dist/ یا *.deb بین‌شان باشد

git commit -m "ClipNest 0.7.0: clipboard history for GNOME on Wayland"

# یکی از این دو؛ اولی با کلید SSH، دومی با توکن (Settings → Developer settings →
# Personal access tokens → Fine-grained: دسترسی Contents روی همین مخزن)
git remote add origin git@github.com:<account>/clipnest.git
git remote add origin https://github.com/<account>/clipnest.git

git remote -v               # ببین آدرس درست است

git tag -a v0.7.0 -m "ClipNest 0.7.0"
git push -u origin main
git push origin v0.7.0      # همین push ورک‌فلوی انتشار را راه می‌اندازد (بخش ۴)
```

اگر احراز هویت قبلاً یک‌بار انجام شده باشد (`git config --global user.name/email` و
کلید یا credential helper)، همین کافی است؛ وگرنه گیت خودش آدرس/یوزرنیم/توکن را
می‌پرسد.

**۳. نتیجه را ببین.** صفحهٔ `Actions` مخزن: ورک‌فلوی `CI` روی `main` (clippy + تست‌ها
روی `ubuntu-24.04`، به‌علاوهٔ یک شاخهٔ arm64 که فقط در مخزن عمومی وجود دارد) و ورک‌فلوی
`Release` روی تگ `v0.7.0` که بسته‌ها را می‌سازد و به Releases می‌چسباند.

> اگر فقط سورس را می‌خواهی و بسته نمی‌خواهی، تگ را نزن؛ `push` کردن `main` کافی است
> و `Release` بدون تگ اجرا نمی‌شود.

## ۲) ساخت بسته‌ها روی همین سیستم

```bash
make dist        # → dist/
ls -lh dist
```

`make dist` اول `make check` می‌زند و بعد این‌ها را می‌سازد:

| مسیر | چیست |
| --- | --- |
| `ubuntu-24.04-amd64/clipnest_<version>-1_amd64.deb` | بستهٔ دبیان/اوبونتو. نام پوشه از خود بسته خوانده می‌شود (`libc6 (>= 2.39)`)، پس اگر روی اوبونتو جدیدتر بسازی هم در همین پوشه می‌نشیند |
| `ubuntu-22.04-amd64/README.md` | توضیح این‌که چرا چنین بسته‌ای ممکن نیست (GTK 4.8 و Shell 45 لازم است) |
| `tarball/clipnest-<version>-<arch>-linux-gnu.tar.gz` | بدون root، هر توزیعی با همین معماری (`./install.sh`) |
| `source/clipnest-<version>.tar.gz` | سورس، برای هر معماری/توزیعی |
| `fedora-<arch>/clipnest.spec` | اسپک RPM، با نسخه و نام حساب پر شده |
| `arch-<arch>/PKGBUILD` | نسخهٔ آرچ، با sha256 تارْبال سورس |
| `linux-arm64/README.md` | چرا این‌جا نیست و چه چیزی می‌سازدش |
| `README.md` | راهنمای همین پوشه (کدام فایل برای کدام سیستم) |
| `SHA256SUMS` | برای `sha256sum -c SHA256SUMS` |

قبل از انتشار، خودت یکی را نصب کن و ببین کار می‌کند:

```bash
make dist-verify                                   # sha256 + محتویات تارْبال + Depends بسته
make deb-verify                                    # بسته را بدون نصب بازرسی می‌کند
sudo apt install ./dist/ubuntu-24.04-amd64/clipnest_<version>-1_amd64.deb
clipnest setup && clipnest doctor
```

`make dist-verify` همان چیزی را می‌گیرد که بسته‌بند نمی‌گیرد: نام همهٔ فایل‌هایی که اسپک RPM
و `install.sh` نصب می‌کنند باید واقعاً داخل تارْبال باشد. اسپک یک بار یونیت کاربری
(`%h/.local/bin/...`) را در `/usr/lib/systemd/user/` می‌گذاشت، یعنی بسته‌ای که دیمنش هرگز
بالا نمی‌آمد؛ حالا همین هدف جلوی تکرارش را می‌گیرد.

## ۳) معماری‌ها و توزیع‌های دیگر

هیچ‌کدام از این‌ها روی همین سیستم ممکن نیست، چون باینری باید با کتابخانه‌های همان
توزیع/معماری لینک شود. ورک‌فلوی `release.yml` همه را می‌سازد (بخش ۴)؛ ولی دستی هم:

**arm64 (.deb و تارْبال)** — روی یک ماشین arm64 (یا رانر arm گیت‌هاب):

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libsqlite3-dev
make dist-deb dist-tarball
```

**RPM (فدورا/اپن‌سوزه)** — روی فدورا، یا با یک container:

```bash
podman run --rm -v "$PWD:/w" -w /w fedora:latest bash -c '
  dnf install -y git tar gzip rpm-build && make dist-tarball dist-extra &&
  rpmbuild -bb dist/fedora-x86_64/clipnest.spec \
           --define "_sourcedir $PWD/dist/tarball" \
           --define "_rpmdir $PWD/dist/rpm" &&
  mv dist/rpm/*/*.rpm dist/fedora-x86_64/'
```

اسپک باید جای‌گذاری‌شده باشد (`make dist-extra` این کار را می‌کند): در آن
`Version: @VERSION@` است و rpmbuild نمی‌تواند آن را باز کند — `--define "version …"`
هم کمکی نمی‌کند، چون تگ `Version` یک مقدار ثابت است نه ماکرو. (ورک‌فلوی انتشار
قبلاً همین‌جا اشتباه می‌کرد و jobهای RPM می‌شکستند.)

اسپک عمداً **باینری را بسته‌بندی می‌کند** نه این‌که بسازد: باینری روی قدیمی‌ترین
نسخهٔ پشتیبانی‌شده ساخته شده تا کفِ `glibc` بالا نرود، و ساختن دوباره روی فدورای جدید
همان کف را بی‌دلیل بالا می‌برد.

**آرچ:** `PKGBUILD` را از `dist/` بردار و کنار تارْبال سورس بگذار:

```bash
makepkg -si
```

**هر چیز دیگر:** سورس را بده و بگذار روی همان ماشین ساخته شود:

```bash
tar xf clipnest-<version>.tar.gz && cd clipnest-<version>
make && make install        # بدون root؛ هرچه لازم دارد را قبلش نصب کن
```

> کفِ واقعی پشتیبانی از خودِ بسته بیرون می‌آید: `dpkg-deb -f *.deb Depends` چیزی مثل
> `libc6 (>= 2.39), libgtk-4-1 (>= 4.8), libadwaita-1-0` چاپ می‌کند. یعنی Ubuntu
> 24.04 و جدیدتر (GNOME 46+). Ubuntu 22.04 (GNOME 42) نه.

## ۴) انتشار یک نسخه

1. نسخه را در **دو** جا بالا ببر: `version` در `Cargo.toml` و یک بخش تازه در
   `CHANGELOG.md`. نسخهٔ اکستنشن (`extension/metadata.json`) را هم اگر فایل JS
   عوض شده بالا ببر، وگرنه gnome-shell نسخهٔ قدیمی را در حافظه نگه می‌دارد.
2. کامیت کن و تگ بزن:

```bash
make check && make dist                     # قبل از تگ، همین‌جا تست و ساخت
git add -A && git commit -m "ClipNest 0.7.0"
git tag -a v0.7.0 -m "ClipNest 0.7.0"
git push origin main && git push origin v0.7.0
```

3. ورک‌فلوی `Release` با دیدن تگ `v*` این‌ها را می‌سازد و به همان تگ می‌چسباند:
   - `clipnest_<v>-1_amd64.deb` و `…-1_arm64.deb` (روی رانر `ubuntu-24.04` و رانر arm، پس کفِ glibc همان 2.39 می‌ماند)
   - `clipnest-<v>-1.x86_64.rpm` و `…-1.aarch64.rpm` (با `rpmbuild` خودِ فدورا، از همان تارْبال)
   - `clipnest-<v>.tar.gz` (سورس) + `PKGBUILD` + `clipnest.spec` + `README.md`
   - `clipnest-<v>-<arch>-linux-gnu.tar.gz` (تارْبال قابل‌حمل)
   - `SHA256SUMS`
4. صفحهٔ Releases را نگاه کن؛ اگر فایلی جا افتاده بود، لاگ همان job را ببین.

**بستهٔ نصب داخل درخت مخزن نمی‌آید و نباید بیاید.** `/dist` عمداً در `.gitignore`
است: گیت برای فایل‌های باینری ساخته نشده و هر بار که بسته را دوباره بسازی، یک نسخهٔ
کامل دیگر از یک فایل ۲۸۸ کیلوبایتی تا ابد در تاریخچه می‌ماند. جای بسته، تب
**Releases** همان تگ است؛ کاربر آن‌جا دانلودش می‌کند و `SHA256SUMS` هم کنارش است.

> اگر با این حال می‌خواهی `.deb` را کنار سورس هم داشته باشی (مثلاً برای این‌که
> لینک raw داشته باشد)، آگاهانه و تک‌فایل اضافه‌اش کن:
> `git add -f dist/ubuntu-24.04-amd64/clipnest_<v>-1_amd64.deb`. `-f` لازم است چون
> فایل ignore شده است.

### اگر خواستی فایل‌ها را دستی هم بگذاری

چیزی که خودت با `make dist` ساختی (و تست کرده‌ای) معمولاً بهتر از نبودنش است، حتی
وقتی CI هم همان‌ها را می‌سازد:

```bash
gh release view v0.7.0                 # اگر نبود:
gh release create v0.7.0 --title "ClipNest 0.7.0" --generate-notes

gh release upload v0.7.0 dist/ubuntu-24.04-amd64/*.deb \
                          dist/tarball/*.tar.gz \
                          dist/source/*.tar.gz \
                          dist/SHA256SUMS
```

بدون `gh` هم می‌شود: در صفحهٔ Releases روی `Edit` همان تگ بزن و فایل‌ها را با موس
بکش داخل کادر، بعد `Update release`.

> `--clobber` (در ورک‌فلو) فایل هم‌نام را جای‌گزین می‌کند؛ دستی هم اگر دوباره آپلود
> کنی همان اتفاق می‌افتد و آپلود دوم برندهٔ اول می‌شود.

**نکتهٔ arm64:** رانر `ubuntu-24.04-arm` برای **مخزن عمومی** رایگان است و برای مخزن
خصوصی وجود ندارد؛ به همین دلیل jobهای arm64 با `continue-on-error` علامت خورده‌اند تا
نبودنشان کل انتشار را زمین نزند. اگر مخزنت خصوصی است و arm64 می‌خواهی، روی یک ماشین
arm64 `make dist-deb dist-tarball` بزن و فایل‌ها را دستی به همان release آپلود کن.

## ۵) بعد از انتشار

- کاربر: `sudo apt install ./clipnest_<v>-1_amd64.deb` → `clipnest setup` →
  یک‌بار logout/login → `clipnest doctor`.
- هر کسی که اوبونتوی قدیمی‌تر دارد، `clipnest-<v>.tar.gz` را می‌گیرد و خودش
  `make && make install` می‌زند.
- برای بررسی درستی دانلود: `sha256sum -c SHA256SUMS`.
- اگر `doctor` روی سیستم کاربر چیزی گفت، همان خروجی diagnose کافی است: هر خط ✅/⚠️/❌
  است و کنار هر ایراد دستورش نوشته شده.
