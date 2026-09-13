# Quy Trình Chuẩn Việt Hóa Game (Dự án RZ - PCSG00933)

Tài liệu này xác định quy trình chuẩn cho quá trình Việt hóa với `rz-tool` và `vwf_patcher` theo ownership hiện tại của hai công cụ.

## Nguyên tắc chung

- `font.tbl` là nguồn mapping `glyph_id -> Unicode` dùng xuyên suốt cho LT/VWF/SC.
- `lt_build_helper.py` sinh `lt-project/font-widths.json` từ chính font/metrics dùng để rasterize glyph. Đây là width artifact nên dùng chung cho runtime VWF và build-time wrapping. `font.cnf` chỉ còn là lớp override/tuning tùy chọn.
- `vwf_patcher` sở hữu hook VWF, width table, speaker VWF và runtime glyph limit.
- `rz-tool build lt` sở hữu allocation/load-size/sector-count của `lt.bin`.
- `rz-tool build sc` sở hữu allocation/metadata/script-buffer sizing của `sc.cpk`.
- `extract-lt-alloc` và `extract-sc-alloc` dùng cho **re-extract asset đã được build/patch**, không phải bước bắt buộc khi trích xuất asset stock lần đầu.
- EBOOT output của bước trước phải được dùng làm EBOOT input của bước sau. Không quay lại EBOOT cũ giữa pipeline.

## Giai đoạn 1: Xử lý Font và VWF (Variable Width Font)

Do font gốc của game tiếng Nhật không chứa đủ các ký tự tiếng Việt, cần mở rộng LT project, patch runtime VWF và sau đó để `rz-tool` cập nhật allocation của `lt.bin`.

### Bước 1.1 - Trích xuất Font

#### Trường hợp bắt đầu từ `lt.bin` stock

Không cần Allocation Map:

```bash
rz-tool extract lt.bin lt-project
```

#### Trường hợp re-extract một `lt.bin` đã được sửa/build trước đó

Phải lấy Allocation Map từ đúng EBOOT đi kèm với `lt.bin` đó:

```bash
rz-tool extract-lt-alloc eboot_patched_lt.elf lt-allocation.json
rz-tool extract lt.bin lt-project --allocation-map lt-allocation.json
```

Không dùng `extract-lt-alloc` trên EBOOT mới chỉ qua `vwf_patcher` nhưng chưa qua `rz-tool build lt`: runtime glyph limit có thể đã tăng trong khi LT allocation vẫn còn stock, và map như vậy không hợp lệ.

### Bước 1.2 - Mở rộng/cập nhật LT project

Dùng helper để cập nhật trực tiếp:

- `lt-project/lt-atlas.png`
- `lt-project/lt-font.json.glyph_count`

```bash
python psv-rz-vwf-patcher/tools/lt_build_helper.py \
  --i lt-project
```

Tool không build `lt.bin`.

Theo contract hiện tại:

- `font.tbl` được auto-resolve và là nguồn mapping/profile;
- `font-widths.json` được sinh tự động từ horizontal advance của font đã chọn cho từng glyph;
- `font.cnf` chỉ là override/tuning tùy chọn; explicit width override metric, `[default]` chỉ fallback khi glyph được preserve nhưng không đo được từ font;
- `Arial.ttf` được auto-resolve làm font mặc định;
- nếu cần nhiều font, truyền `--font-file` nhiều lần theo thứ tự ưu tiên.

Ví dụ:

```bash
python psv-rz-vwf-patcher/tools/lt_build_helper.py \
  --i lt-project \
  --font-file Arial.ttf \
  --font-file fallback.ttf
```

Nếu muốn chỉnh glyph bằng tay, nên chạy helper trước để mở rộng atlas/profile, sau đó mới sửa trực tiếp `lt-atlas.png`. Không chạy helper lại sau phần chỉnh tay trừ khi muốn rasterize lại các glyph được map.

### Bước 1.3 - Patch EBOOT hỗ trợ VWF

Patch từ EBOOT stock:

```bash
python psv-rz-vwf-patcher/rz_vwf_stock_patcher.py \
  --eboot-in eboot.bin.elf.org \
  --eboot-out vwf_patched_eboot.bin.elf \
  --font-tbl psv-rz-vwf-patcher/examples/font.tbl \
  --width-table lt-project/font-widths.json \
  --speaker-max-width 192
```

`vwf_patcher` ở bước này chịu trách nhiệm cho:

- VWF renderer/hooks;
- runtime width table;
- speaker VWF và speaker pixel limit;
- runtime glyph limit.

`vwf_patcher` **không còn nâng allocation/load-size/sector-count của `lt.bin`**.

### Bước 1.4 - Build `lt.bin` và cập nhật LT allocation trong EBOOT

```bash
rz-tool build lt-project lt.bin \
  --eboot-in vwf_patched_eboot.bin.elf \
  --eboot-out eboot_patched_lt.elf
```

`rz-tool` sẽ:

1. build `lt.bin`;
2. kiểm tra `lt-font.json.glyph_count` khớp runtime glyph limit trong EBOOT;
3. tính allocation cần thiết từ `lt.bin` đã build;
4. cập nhật LT allocation/load-size/sector-count trong EBOOT output.

Trong lần build LT đầu tiên từ đúng `vwf_patched_eboot.bin.elf`, không cần `-f`.

Nếu input EBOOT đã từng bị một LT/SC allocation build khác thay đổi bên trong protected runtime hash range, có thể dùng:

```bash
rz-tool build lt-project lt.bin \
  --eboot-in previous_patched_eboot.elf \
  --eboot-out eboot_patched_lt.elf \
  -f
```

`-f` chỉ bỏ qua mismatch của `VWF_RUNTIME_HASH_RANGE_SHA256`; nó không bỏ qua các kiểm tra `glyph_count`, allocation, alignment hoặc ELF.

## Giai đoạn 2: Trích xuất và Dịch thuật Kịch bản (`sc.cpk`)

Dữ liệu chữ chính của game nằm trong `sc.cpk`.

### Bước 2.1 - Trích xuất Kịch bản

#### Trường hợp bắt đầu từ `sc.cpk` stock

Không cần SC Allocation Map:

```bash
rz-tool extract sc.cpk sc-project \
  --charset-map font.tbl
```

#### Trường hợp re-extract một `sc.cpk` đã được build trước đó

Lấy Allocation Map từ đúng EBOOT tương ứng với `sc.cpk` đó:

```bash
rz-tool extract-sc-alloc patched_eboot.elf sc-allocation.json

rz-tool extract sc.cpk sc-project \
  --charset-map font.tbl \
  --allocation-map sc-allocation.json
```

### Bước 2.2 - Dịch thuật

Chỉnh sửa:

```text
sc-project/scenario-dialogue.json
```

Tập trung vào:

- `text`: nội dung hiển thị;
- `speaker`: tên nhân vật.

Nên chia quá trình dịch thành các batch nhỏ để dễ kiểm tra JSON, marker và wrapping.

## Giai đoạn 3: Build Kịch bản và EBOOT cuối

Sau khi dịch xong:

```bash
rz-tool build sc-project sc_patched.cpk \
  --eboot-in eboot_patched_lt.elf \
  --eboot-out final_eboot.bin.elf \
  --charset-map font.tbl \
  --wrap-width-table lt-project/font-widths.json \
  --wrap-mode word \
  --wrap-rows 3 \
  -f
```

Trong workflow chuẩn của tài liệu này, `-f` là chủ đích vì `eboot_patched_lt.elf` đã được `rz-tool build lt` thay đổi LT allocation bên trong protected runtime hash range.

`-f` **không** có nghĩa là bỏ qua mọi kiểm tra; nó chỉ cho phép tiếp tục khi `VWF_RUNTIME_HASH_RANGE_SHA256` không còn bằng strict hash của standalone VWF-patched EBOOT.

Nếu SC build được chạy trực tiếp trên EBOOT vừa ra từ standalone `vwf_patcher` và chưa có allocation build nào khác, có thể bỏ `-f`.

### Metadata dialogue

Nếu wrapping sinh continuation/marker metadata, `rz-tool` có thể tạo:

```text
sc_patched.cpk.rz-dialogue-meta.json
```

Giữ file này đi kèm `sc_patched.cpk`. Nó được dùng để phục hồi/fold-back dialogue khi re-extract archive đã build.

## Giai đoạn 4: Đóng gói EBOOT (FSELF) cho PS Vita

Input hiện tại là ELF, vì vậy tạo FSELF cuối:

```bash
vita-make-fself.exe -c final_eboot.bin.elf eboot.bin
```

`eboot.bin` là file deploy lên PS Vita.

Không dùng một EBOOT ELF cũ hơn thay cho `final_eboot.bin.elf`, vì EBOOT cuối phải đồng thời chứa:

- VWF runtime patch;
- LT runtime glyph limit;
- LT allocation hiện tại;
- SC allocation/metadata/script-buffer sizing hiện tại.

## Giai đoạn 5: Validation và Re-extract Check

### Bước 5.1 - Validator

```bash
python tools/validate_engine_assets.py \
  --elf final_eboot.bin.elf \
  --secrect secrect.json
```

`--secrect` là spelling hiện tại của CLI validator.

### Bước 5.2 - Kiểm tra lại Allocation Map từ EBOOT cuối

```bash
rz-tool extract-lt-alloc final_eboot.bin.elf lt-allocation.final.json
rz-tool extract-sc-alloc final_eboot.bin.elf sc-allocation.final.json
```

Hai map này nên được lưu cùng build cuối vì chúng mô tả allocation/runtime profile thực tế của EBOOT đã deploy.

### Bước 5.3 - Re-extract test khuyến nghị

Để xác nhận round-trip sau build:

```bash
rz-tool extract lt.bin lt-reextract \
  --allocation-map lt-allocation.final.json
```

và:

```bash
rz-tool extract sc_patched.cpk sc-reextract \
  --charset-map font.tbl \
  --allocation-map sc-allocation.final.json
```

Nếu có:

```text
sc_patched.cpk.rz-dialogue-meta.json
```

hãy giữ nó cạnh archive để `rz-tool` có thể phục hồi continuation dialogue đúng logic.

## Pipeline chuẩn rút gọn

```text
stock lt.bin
    |
    v
rz-tool extract
    |
    v
LT project
    |
    v
lt_build_helper.py --i
    |
    v
lt-atlas.png + lt-font.json.glyph_count

stock eboot.bin.elf
    |
    v
vwf_patcher
    |
    v
VWF-patched EBOOT
    |
    +---------------------+
    |                     |
    v                     |
rz-tool build LT          |
    |                     |
    v                     |
lt.bin + LT-patched EBOOT |
    |                     |
    +----------+----------+
               |
               v
        rz-tool build SC -f
               |
               v
   sc_patched.cpk + final ELF
               |
               v
       vita-make-fself
               |
               v
          deploy/test
```

## Quy tắc quan trọng

1. Dùng cùng `font.tbl` cho LT/VWF/SC charset mapping.
2. Dùng cùng `lt-project/font-widths.json` cho runtime VWF và SC wrapping; chỉ dùng `font.cnf` như override/tuning nếu cần.
3. `vwf_patcher` không sở hữu LT allocation.
4. `rz-tool build lt` luôn nhận `--eboot-in` và `--eboot-out`.
5. Sau mỗi build có EBOOT output, dùng EBOOT output đó làm input cho bước kế tiếp.
6. Chỉ dùng `-f` để bypass runtime hash mismatch có chủ đích khi chain các allocation build.
7. Allocation Map phải được trích từ EBOOT tương ứng với đúng asset đã build/deploy.
8. `extract-lt-alloc` / `extract-sc-alloc` là công cụ re-extract, không phải bước bắt buộc khi bắt đầu từ asset stock.
