# VK Live Transcoder — constraints & capacity-boost ideas

Конспект того, что делает VK Live transcoder со стримом, где остаётся пространство для
полезной нагрузки, и перечень идей по увеличению информационной плотности канала
(bits-per-cell, bits-per-frame, bits-per-second).

Данные — эмпирические замеры из `metrics/live/sweep-results.md` + reverse-engineering по
наблюдениям.

---

## 1. Что делает VK transcoder

### 1.1 Rate control / bitrate
- Вход: принимает publish до ~**8-10 Mbps** на 720p slot.
- Выход: режет к целевым ladder-битрейтам:
  - 720p → **3-4 Mbps**
  - 480p → 1.5-2 Mbps
  - 360p → 800 kbps
  - 144p → 400 kbps
- VBV buffer: резкие всплески ≥2× average → дропы frames.
- **Рабочее окно:** publish 500k-4000k на 720p. Ниже — under-fill artifacts, выше — overshoot без выигрыша.

### 1.2 Spatial downsample / rescale
- Ресэмплит ко внутренней ladder (1080/720/480/360/240/144).
- Kernel: bicubic / Lanczos (оценочно). Высокочастотный spatial сигнал размывается.
- **Рабочее окно:** `cell_size ≥ 8 px` для 720p, `≥ 16 px` для гарантии.

### 1.3 Chroma subsampling
- Выход: почти всегда **yuv420p** (1 U + 1 V на каждые 2×2 Y).
- Chroma resolution вдвое хуже luma по обоим осям.
- **Рабочее окно:** chroma cells должны быть 2× крупнее luma cells. Либо Y-only модуляция в мелких cells + Y+UV в крупных.

### 1.4 DCT quantization (8×8 / 4×4 блоки)
- Q-tables × QP. Высокочастотные coeffs обнуляются первыми.
- **Рабочее окно:** сигнал в DC + первых 2-4 низкочастотных коэффициентах переживает.
  Mid/high frequency убиваются при QP ≥ 22.

### 1.5 Deblocking filter (loop filter)
- Сглаживает границы 4×4 / 8×8 блоков внутри transcoder. Мы можем отключить `no-deblock=1` в своём encoder, но VK re-applies.
- Центры cells сохраняют информацию лучше, чем края.
- **Рабочее окно:** читать только центральную зону cell (уже реализовано: `read_offset`, `read_size`).

### 1.6 GOP / keyframe policy
- I-frame interval: ~2-4 sec. VK может перекодировать GOP.
- P/B frames используют inter-prediction → статичные области копируются между frames почти бесплатно.
- Меняющийся каждый frame сигнал стоит encoder'у дороже → хуже качество остаётся для нашего сигнала.
- **Рабочее окно:** держать cell value **2-3 frames подряд** → data-rate 8-12 fps при video 24 fps.

### 1.7 Motion estimation
- Ищет похожие блоки в предыдущем frame → motion vector + residual.
- Резкие переходы cell value → motion estimator промахивается → bits тратятся впустую.
- **Рабочее окно:** ограничить допустимые переходы cell values (близкие цвета → маленький residual).

### 1.8 Color space conversion
- RGB → YUV (BT.601 / BT.709) на входе, back to RGB на клиенте.
- Не все YUV-точки валидны как RGB → **clipping ~30%** точек при uniform grid.
- **Рабочее окно:** palette strictly inside RGB gamut. Optimal packing даёт 0% потерь.

### 1.9 8-bit precision + sRGB gamma
- Output: 8 bit per channel.
- Дискретизация плотнее в тенях, реже в светах (sRGB non-linear).
- **Рабочее окно:** использовать midtones (Y = 80..180). 16 различимых уровней luma в этом диапазоне.

### 1.10 Temporal filtering / denoising
- Пиксельное усреднение между frames для borba с шумом.
- Быстрые flicker-паттерны размазываются → амплитуда падает.
- **Рабочее окно:** менять cell не чаще чем раз в 2 frame, либо использовать высокоамплитудные переходы (ΔY ≥ 30).

### 1.11 Scene-change artifacts
- Глобальная смена всех cells → bitrate spike → VK режет → грязь на 5-10 frames.
- **Рабочее окно:** delta-encoding cells (меняется часть cells, остальные stable).

### 1.12 HRD / latency
- VK добавляет 300-1000 ms latency сам по себе.
- Полный RTMP→ingest→transcode→HLS путь: 3-5 sec.
- Учтено в warmup/retry logic.

### 1.13 Audio lane
- AAC 128 kbps, почти не трогается transcoder'ом (только sample-rate re-encode).
- Потенциал: ultrasonic band 18-22 kHz как side-channel, +50-500 bps.

### 1.14 Watermarks / overlays
- VK иногда накладывает logo в верхний-правый угол ≈ 80×40 px.
- **Рабочее окно:** резервировать эту зону (не размещать cells).

---

## 2. Физические пределы канала (Shannon)

На один cell × frame после VK transcode:

| аспект | оценка |
|---|---|
| Luma levels различимых после quantization | 16 (midtones), 8 (края) |
| Chroma levels различимых (U, V) | 4-8 каждый |
| SNR per cell (измеренный) | ~21 dB |
| Shannon capacity per cell (raw) | ~10-14 bits |
| Shannon capacity × 3 каналов (Y+U+V) | ~21 bits теоретический максимум |
| Текущий Mode C (4 bits/cell) | 4 bits |
| Практически достижимо с best-in-class FEC | 12-14 bits/cell |

**Фундаментально:** один 4×4 region × 1 frame через VK transcoder физически не может
унести > ~20 bits. Всё что выше — переопределение «cell» в сторону time/space pooling.

---

## 3. Идеи по увеличению плотности

### 3.1 Цветовой alphabet (прямой constellation growth)

| Mode | палитра | bits/cell | риск |
|---|---|---|---|
| B (текущий) | 4 уровня Y (0/85/170/255) | 2 | baseline, устойчив |
| C (текущий) | 16 Y×U×V combos | 4 | baseline, устойчив |
| M1 | 8 Y × 4 U × 4 V = 128 точек | **7** | средний, нужна калибровка |
| M2 | 16 Y × 4 U × 4 V = 256 точек | **8** | средний |
| M3 | 16 × 8 × 8 = 1024 точек | **10** | высокий, нужен pilot-based calibration |

**Улучшения палитры:**
- **Optimal packing в валидной RGB gamut** — +0.5 bit effective за счёт нулевых
  clipping-потерь (grid теряет 30% точек).
- **Constellation shaping** (Gaussian-distributed, плотнее к центру) — +0.3 bit
  shaping gain.
- **Perceptual midtone clustering** (избегать тёмного/светлого краёв,
  где банд-дискретизация грубее) — +0.5 bit.
- **Gamma-aware spacing** — учёт sRGB нелинейности для равномерной
  perceptual distance между соседними точками.

### 3.2 Sequence coding (мапинг последовательностей битов)

Базовая идея: cell несёт не фиксированное количество bits, а **один символ**
из большого алфавита. Последовательность input bits → последовательность
символов. То что обсуждали:

- **Arithmetic coding / Huffman** pre-stage: сжимаем input до мин. энтропии
  → меньше символов на тот же data stream. Ноль-эффект на random tunnel payload,
  но для HTTP/text SOCKS traffic — 2-5× compression.
- **zstd / brotli** pre-compression тунелируемого payload: аналогично.
  Бесплатный wins на browse/HTML трафике.

### 3.3 Trellis coded modulation (TCM)

Накладываем «память» между соседними cells: не все пары `(cell_n, cell_{n+1})` валидны,
только определённые переходы по trellis-диаграмме.

- Decoder использует Viterbi алгоритм: даже если одна cell прочиталась
  неоднозначно, соседи говорят «это состояние недостижимо» → берём второй best.
- **Coding gain:** 3-6 dB = **+1-2 bits/cell** на тот же noise floor.
- Стандарт DSL / WiFi / 4G.

### 3.4 Soft-decision FEC

Сейчас decoder выдаёт bit=0 / bit=1 / erasure. **Soft decision** передаёт
log-likelihood ratio (LLR) в FEC decoder.

- **Gain:** 2-3 dB → +0.5-1 bit/cell.
- **Change:** RS → LDPC или Polar с soft input.
- `confidence` уже вычисляется в `read_cell`, просто выбрасывается при decode — **free win**.

### 3.5 Multi-level coding

Разные FEC на разных каналах:
- Y-bits: weak code (4:2:0 = 4 samples на cell → самые надёжные).
- U/V-bits: strong code (меньше samples → больше ошибок).

Gain: +0.3-0.5 bit / cell.

### 3.6 Bit-level interleaving

Сейчас interleaving по cells. Если bits одного codeword разбросать **по cells + по
Y/U/V каналам**, burst errors от VK deblock не выкосят целый codeword.

Gain: +0.2-0.5 bit/cell в burst-scenarios (т.е. реальных).

### 3.7 Temporal stacking (pseudo-cell over time)

Не per-frame, a **per-position поток frames**:
- cell[x,y] держит одно value 2-3 frames подряд (inter-prediction экономит bitrate
  encoder'а → ему остаётся больше bits на качество).
- data-rate = video_fps / 2..3 = 8-12 data-fps при 24 video-fps.
- Per spatial-position в секунду: 10 bits × 8 fps = **80 bits/sec/position**.

### 3.8 DCT-coefficient stuffing

Вместо «cells как прямоугольники», встраивать сигнал в **низкочастотные DCT
коэффициенты** 16×16 / 32×32 блоков.

- DC + первые 2-4 coeff'а переживают transcode даже при QP=30.
- Низкочастотные coeffs не квантуются агрессивно (они несут основную энергию).
- Потенциал: **9-20 bits per 16×16 block** robust, до **40+ bits** при жёсткой
  калибровке.
- 720p → 2700 блоков × 15 bits = 40 kbit/frame × 24 fps = **~1 Mbps**.
- **Эффект:** 10× прирост capacity над пикcel-domain модуляцией.
- **Цена:** 3-4 недели работы, полностью новый codec.

### 3.9 Motion-vector steganography

Инжектить бит-информацию в MV residuals (они сохраняются bitstream'ом почти
без потерь).

- 1 MV per 16×16 macroblock × 8 bits =  **64 bits/macroblock** = 4 bits/cell
  при cell=4.
- **Цена:** перехват x264 на encoder API уровне.

### 3.10 Semantic / cover encoding (радикальное)

Кодировать bits не в пикселях, а **в семантике кадра** — заранее согласованной
library of scenes (персонаж держит флажок N из 1024 вариантов, фон X из 256).

- Transcoder обязан сохранить object identity → тысячи bits per frame.
- **Цена:** требует ML или rule-based scene rendering, полностью другая
  архитектура.

### 3.11 CDMA / spread-spectrum per-cell

Наложить M ортогональных PRN-кодов на один cell. Decoder с known codes делает
correlation → восстанавливает все M bits параллельно.

- **Реальный эффект:** capacity = log2(SNR), просто перераспределена между
  «виртуальными каналами». **Не прирост**, переупаковка. Может быть полезно
  для robustness при burst errors.

### 3.12 Sub-pixel chroma offset

Кодировать bits в полупиксельном сдвиге chroma sample (VK ресэмплер
сохраняет subpixel edges как soft gradient).

- +2-4 bits per chroma sample × 4 chroma samples / cell = **+16 theoretical,
  +4-8 realistic**.

### 3.13 LDPC / Polar long-block codes

RS(172,120) → LDPC over 1000 frames (~42 sec).

- **Gain:** +3-4 bits/cell за счёт длинных кодов (burst errors усредняются).
- **Цена:** latency 42+ sec. Несовместимо с interactive SOCKS.

---

## 4. Ранжир по gain-vs-effort

| техника | +bits/cell | effort | рекомендация |
|---|---|---|---|
| Mode M2 (256 colors, 8 bits) | +4 | 1-2 дня | **Stage 1 — делаем** |
| Mode M3 (1024 colors, 10 bits) | +6 | 3-5 дней | **Stage 1** |
| Optimal palette packing | +0.5 | 1 день | **Stage 1** |
| Adaptive pilot calibration | +1-2 robust | 3-4 дня | **Stage 1** |
| Soft-decision LDPC вместо RS | +0.5-1 | 1 неделя | **Stage 2** |
| TCM (trellis coded modulation) | +1-2 | 2 недели | **Stage 2** |
| Multi-level FEC + bit interleaving | +0.5 | 3-4 дня | **Stage 2** |
| Constellation shaping | +0.3 | 2 дня | Stage 2 (опц.) |
| Temporal stacking | формально +80/sec | 1 день | **Stage 1 (быстрый win)** |
| Pre-compression (zstd на tunnel) | 2-5× для text | 1 день | **Stage 1 (бесплатный win)** |
| DCT-coefficient stuffing | +15-20 (per cell) | 4 недели | Stage 3 (радикально) |
| MV steganography | +4 | 2 недели | Stage 3 |
| Semantic cover encoding | 1000+/frame | 4-8 недель | Out of scope |
| LDPC long-block | +3-4 | 1 неделя | отвергаем (latency) |
| CDMA per-cell | 0 | — | не делаем |

---

## 5. Рекомендуемый путь

**Stage 1 (2-3 дня, цель: ×3 throughput):**
1. zstd pre-compression туннелируемого payload.
2. Mode M2 (256 colors) с optimal packing в RGB gamut.
3. Adaptive pilot-based YUV calibration на приёмной стороне.
4. Temporal stacking (cell value held 2 frames для лучшего inter-prediction).

Ожидаемый результат: с 4 bits/cell × 1× compression → **8 bits/cell × 2× compression
= 4× эффективная capacity**.

**Stage 2 (1-2 недели, цель: ещё ×1.5-2):**
1. Soft-decision LDPC (замена RS).
2. TCM слой поверх constellation.
3. Multi-level code на Y vs U/V.

Ожидаемый результат: **12-14 bits/cell** effective, близко к Shannon.

**Stage 3 (опционально, 4+ недель, цель: ×10):**
1. DCT-domain steganography.

Ожидаемый результат: переход от covert-signalling к near-lossless watermark.

---

## 6. Абсолютные пределы

- **Per-cell per-frame физический потолок:** ~20 bits (Shannon через VK lossy canal).
- **Per-cell per-second потолок** (с temporal stacking): ~200-400 bits.
- **Per-frame потолок** при оптимальном cell_size=4 на 720p (57600 cells):
  57600 × 14 bits = 807 kbit/frame × 24 fps = **~19 Mbit/s теоретический максимум**.
  VK bitrate ladder этого не пропустит — реальный потолок publish 4 Mbps × efficiency
  0.5 = **~2 Mbit/s effective**.
- **Семантический путь** обходит этот потолок (signalling через meaning,
  не через pixels), но это уже другой протокол.
