# cfgprism — DESIGN (Этап 0. Разведка)

> Статус: Этап 0 завершён, кода нет. Этот документ — единственная правда
> о выборе парсеров и архитектуре. Любые противоречия с ТЗ решаются
> через `docs/DECISIONS.md`.

## 0. Цель проекта (напоминание)

Конвертер конфигов между форматами, который **сохраняет комментарии,
порядок ключей и форматирование** там, где целевой формат умеет их выразить,
и **явно предупреждает** (`Warning { path, kind, message }` → stderr,
`--strict` превращает в ошибку) там, где потеря неизбежна.
Антипод: `yq`/`dasel` молча нормализуют или теряют trivia.

## 1. Что изучено

### 1.1. yq (mikefarah/yq, Go)

- Парсер: `gopkg.in/yaml.v3` (ранее `goccy/go-yaml` в экспериментах).
  `yaml.v3` хранит `HeadComment/LineComment/FootComment`, стиль, якоря/теги —
  поэтому `yq eval -i` сохраняет комментарии при **обновлении на месте**.
- Слабости для нашей задачи:
  1. Конвертация (`-o=json`) **молча дропает** комментарии/стили/якоря.
     Никаких warnings, никакого `--strict`.
  2. `yq` — это query-процессор (jq-синтаксис), а не конвертер с IR.
     Его модель «декодируй в Node → перекодируй» нормализует whitespace,
     порядок при `sort_keys` и т.п.
  3. Известные баги whitespace/comments
     (issues #718, CRLF #1871) упираются в ограничения `go-yaml`.
- Вывод: подтверждаем нишу cfgprism — не конкурировать с jq-синтаксисом,
  а дать **честную кросс-конвертацию с warnings**. У yq заимствуем UX
  (`-o`, `eval`-идеи не нужны; нам нужен только `convert`).

### 1.2. dasel (TomWright/dasel v3, Go)

- Архитектура: каждый формат парсится в **общую generic-модель**
  (`model.Value`), trivia **отбрасывается на входе**.
  Man-страница прямо фиксирует: «Comments in YAML and TOML files are
  discarded when writing due to parser limitations».
- Плюсы: широкая матрица форматов (JSON/YAML/TOML/XML/CSV/HCL/INI/KDL),
  единый селектор, удобно как референс CLI (`-i/-o`, stdin/stdout).
- Вывод: dasel — антипример для IR. Наш IR обязан хранить trivia
  с первого дня, иначе повторим его потери. Форматную матрицу и флаги
  `-i/-o` копируем как UX-ориентир.

### 1.3. TOML: `toml_edit` vs `taplo`

| Критерий | `toml_edit` (toml-rs) | `taplo` (tamasfe) |
|---|---|---|
| Trivia | Да: комментарии, пробелы, относительный порядок; `DocumentMut::to_string()` даёт byte round-trip | Да: полный rowan green-tree, каждый символ сохранён |
| Ошибки с позицией | Да: `TomlError::span()`, line:col вычисляется | Да: offsets + lengths |
| Вес зависимостей | Лёгкий, без rowan/logos | Тяжёлый: `rowan, logos, itertools, tracing…`, ~160K SLoC транзитивно |
| Спецификация | Актуален, spec-1.1.0, MSRV 1.85, 300M+ загрузок | Актуален, но ориентирован на LSP/форматтер/схемы |
| API для конвертера | `DocumentMut/Item/Table/Value + decor/repr` — удобно маппить в IR | DOM + синтаксическое дерево — мощно, но избыточно |

**Решение: `toml_edit`.** Причины: byte round-trip из коробки,
минимальный dependency-tree (важно для «один статический бинарник» и WASM),
API прямо даёт decor (prefix/suffix whitespace + comments) и repr
(кавычки, radix чисел, datetime-форматы) — это ровно наши `Trivia`+`Style`.
`taplo` отклонён: rowan-дерево — оверхед для конвертера, тянем только если
понадобится LSP/форматтер позже. Ограничение `toml_edit` (порядок dotted-keys,
reorder scattered `[tables]` — см. README крейта) фиксируем как известные
потери и покроем warning-ами/тестами.

### 1.4. YAML: `yaml-rust2` / `saphyr` / `serde-saphyr` / `granit-parser`

Проверено по docs.rs и исходникам:

- `yaml-rust2` (форк `yaml-rust`, libyaml-наследие): парсит в `Yaml::Hash`
  (`LinkedHashMap`-подобный) / `Array`, **комментарии, стили скаляров,
  якоря (кроме alias-разрешения), исходные кавычки — теряются**.
  `YamlEmitter` нормализует вывод. Низкоуровневые `Event` есть, но без spans
  и trivia. Статус: **только basic maintenance**, новые фичи идут в `saphyr`.
- `saphyr` + `saphyr-parser`: полностью YAML 1.2 compliant, быстрее и
  корректнее, но модель та же — **комментариев нет**. `saphyr-parser`
  отдаёт поток `Event` (стили, якоря/алиасы, теги есть), но не комментарии
  и не точные spans под round-trip.
- `serde-saphyr` (на `granit-parser`, форке saphyr): единственный, кто
  поднял комментарии — wrapper `Commented<T>` захватывает/эмитит inline
  комменты, есть `Budget`/spans. Но: **freestanding-комментарии не
  захватываются** («use granit-parser directly»), flow-контексты подавляют
  комменты, семантика привязки хрупкая.
- Вывод: **готового trivia-preserving YAML-крейта уровня `toml_edit` в
  Rust нет** (аналога `hcl-edit`/`kdl` для YAML не существует).

**Решение: свой YAML-слой поверх `saphyr-parser`.**
`saphyr-parser` — источник событий (mapping/sequence/scalar, стили,
якоря/алиасы, теги, multi-doc границы) как compliant-парсер 1.2.
Поверх — собственный коллектор trivia: предпроход сканером исходника
собирает `#`-комментарии, пустые строки, отступы, block-стили (`|/>`),
flow-стили (`{}/[]`), кавычки — и аттачит к IR-узлам по строкам.
Альтернатива «взять `yaml-rust2`» отклонена: менее активен, та же потеря
trivia, а compliance хуже. `unsafe-libyaml` отклонён: C-зависимость ломает
«один статический бинарник» и WASM. Это самый рискованный узел проекта,
поэтому Этап 3 выделен целиком под YAML + отдельный набор Norway-тестов.

### 1.5. JSON / JSONC / JSON5

- `serde_json`: быстрый, но **без комментариев**; порядок — только с фичей
  `preserve_order` (IndexMap). Для round-trip не годится.
- `jsonc-parser` (dprint): парсит JSONC в `JsonValue` **и** в CST/AST с
  `comments: true, tokens: true`, есть spans. Ближе всего к нужному.
- `json5-rs` (callum-oakley), `serde_json5` (google-форк), `json-five-rs`:
  все **serde-ориентированы, trivia дропают**. `json5format` (google):
  форматирует JSON5 **с сохранением contextual line/block comments** —
  полезно как референс эмиттера, но не полный парсер под IR.
- **Решение:**
  - JSON (strict): собственный тонкий парсер поверх идеи `jsonc-parser`
    либо напрямую `jsonc-parser` с включёнными tokens/comments; выбор
    финализируется на Этапе 2 бенчмарком, дефолт — **свой hand-rolled
    recursive-descent**, чтобы гарантировать byte round-trip и spans
    без surprises CST-API.
  - JSONC/JSON5: собственный парсер (расширение JSON: `//`,`/* */`,
    trailing commas, unquoted keys, single-quotes, hex, multiline strings,
    `+`-знак, `.5`). Причина: ни один Rust-крейт не даёт
    trivia-preserving JSON5. Грамматика маленькая, писать самим дешевле,
    чем форкать.

### 1.6. `.env` / INI / Properties

- `.env`: `dotenv/dotenvy` — только loader в `env`, комментарии/порядок/
  кавычки/`export` теряются; `dotenv-parser` — заброшен (5+ лет, ISC,
  только BTreeMap). Грамматика `.env` тривиальна (строки `KEY=VAL`,
  `export`, `#`, кавычки, continuations).
  **Решение: свой line-based парсер** (~200 строк), сразу с trivia.
- INI: `rust-ini`, `configparser` —丢 trivia/порядок секций/дубликаты
  обрабатывают по-разному; round-trip не гарантируют.
  **Решение: свой парсер** (секции, `key=value|key:value`, `;/#`-комменты,
  continuations). Дешевле, чем чинить чужой.
- Properties (Java): отдельного зрелого trivia-крейта нет.
  **Решение: свой парсер** (тот же класс, что INI/`.env`).

### 1.7. Этап 5 (HCL / KDL / RON)

- **HCL: `hcl-edit` (+ `hcl-rs`).** `hcl-edit` — дословно «to HCL what
  `toml_edit` is to TOML»: хранит whitespace/comments, API вдохновлён
  `toml_edit`. `hcl-rs` сверху даёт serde + eval expressions/templates.
  Ограничение: native-syntax expressions (`for`, functions, `${}`) в IR
  маппятся как opaque-скаляры + warning `ExprOpaque`. Scope Этапа 5 —
  «упрощённый HCL» (атрибуты/блоки/литералы), полный eval — вне scope.
- **KDL: `kdl` (kdl-rs, v2).** Document-oriented, «`toml_edit`, но для KDL»:
  хранит formatting/whitespace/comments, byte round-trip из коробки,
  v1/v2 + конвертация между ними. `knus` отклонён: он serde-derive,
  trivia дропает. **Решение: `kdl`.**
- **RON: `ron` (ron-rs).** Умеет comments/trailing commas/enums, но
  **не trivia-preserving** (нет spans/round-trip API); `ron2` (AST-based,
  2026) незрел (4 звезды, unstable API). **Решение: `ron` для значений +
  собственный pre-lex комментариев с аттачем к IR; byte round-trip для RON
  НЕ гарантируем** (фиксируем в таблице поддержки как `round-trip: partial`).
  Если `ron2` стабилизируется — мигрировать.

## 2. Архитектура

### 2.1. Крейты

```text
cfgprism-core    — IR (Node/Doc/Trivia/Style/Span), trait Format, Warning, convert()
cfgprism-formats — модули json.rs, jsonc.rs/json5.rs, toml.rs, dotenv.rs, ini.rs,
                   (этап 3) yaml.rs, (этап 5) hcl.rs, properties.rs, kdl.rs, ron.rs
cfgprism-cli     — бинарь `cfgprism convert <in> [-f FROM] -t TO [-o OUT] [--strict]`
cfgprism-wasm    — wasm-bindgen обёртка для веб-демо (этап 7)
```

Правила зависимости: `formats → core`, `cli → core+formats`,
`wasm → core+formats`. Ядро **ничего не знает** о конкретных форматах —
иначе внешние контрибьюторы не смогут добавлять форматы «не трогая ядро»
(требование Этапа 5).

### 2.2. IR

```rust
struct Doc  { root: Node, trailing: Trivia, … }
struct Node {
  key: Option<Key>,        // для map-entries; хранит исходный repr ключа
  value: Value,            // Null|Bool|Num|Str|Array|Map
  order: usize,            // исходный порядок (IndexMap-инвариант)
  trivia: Trivia,          // leading comments, inline comment, blanks_before, trailing
  style: Style,            // Quoted{single|double}|Plain|Block{literal|folded}|Flow{…}|Original(repr)
  span: Option<Span>,      // line:col начала/конца в исходнике
  anchor: Option<Anchor>,  // YAML-якоря/алиасы (None вне YAML)
}
struct Warning { path: String /* JSON-pointer-ish */, kind: WarningKind, message: String }
enum WarningKind { CommentDropped | AnchorExpanded | StyleNormalized | TypeCoerced
                 | KeyReordered | LossyNumber | UnsupportedConstruct | … }
trait Format {
  fn name(&self) -> &'static str;
  fn parse(&self, src: &str) -> Result<Doc, Error>;   // Error всегда с line:col
  fn emit(&self, doc: &Doc, opt: &Options) -> Result<String, Error>;
}
fn convert(doc: &Doc, from: &dyn Format, to: &dyn Format) -> (String, Vec<Warning>)
```

- Порядок: `Map` — всегда `IndexMap`-семантика, сортировки нет (кроме
  явной опции эмиттера, которая обязана дать warning `KeyReordered`).
- Числа/даты: храним **и** типизированное значение, **и** исходный repr
  (`Original`), чтобы `01` vs `1`, `0o17`, `no` vs `"no"`, даты не
  перетирались молча. Неоднозначности — в warning `TypeCoerced`.
- `trivia` — полноправная часть IR, а не second-class: каждый формат,
  который не умеет выразить trivia-приёмника, обязан вернуть warning,
  а не дропнуть молча. JSON (strict) — канонический пример:
  комментарии → `CommentDropped`, если цель — plain JSON, а не JSONC/5.

### 2.3. Конвертация и warnings

Матрица Этапа 4: для каждой пары `(from, to)` — golden-тесты +
таблица «что теряется». Таксономия warnings фиксирована в ядре
(`WarningKind` — non_exhaustive, чтобы форматы могли добавлять свои,
но базовые виды стабильны для `--strict` и для веб-демо).
`--strict` = любой warning → `exit != 0` + текст в stderr.

### 2.4. Тестирование (задел под Этапы 2–4)

- `tests/fixtures/<format>/*.in` + `*.out` — golden files; round-trip
  «в себя без изменений байт» для Этапа 2 (кроме задокументированных
  исключений `toml_edit`: dotted-keys/reordered scattered tables).
- Этап 3: Norway-problem (`no/on/off/yes` как строки vs bool),
  `0o`-числа, даты/timestamps, `sexagesimal`, merge-keys `<<`, multi-doc
  (`---/...`) — каждый кейс отдельным fixture + warning-assert.
- Property: `parse → emit → parse ≡ IR` (эквивалентность без spans).
- Fuzz (`cargo-fuzz`): JSON/TOML/YAML парсеры — только свой код + адаптеры;
  `toml_edit`/`saphyr-parser` фаззим через наши обёртки (ловим паники
  в аттаче trivia).
- Bench: 5 МБ < 1 c. Задел: zero-copy где дёшево, но trivia-аллокаций
  не избегаем ценой корректности; замеряем на Этапе 2.

## 3. Решения (кратко; детали — в DECISIONS.md D1–D8)

| # | Решение | Обоснование (1 строка) |
|---|---|---|
| D1 | TOML — `toml_edit` | byte round-trip + decor/repr + лёгкие зависимости; `taplo` — оверхед |
| D2 | YAML — `saphyr-parser` + свой trivia-collector | нет готового trivia-YAML в Rust; нужен compliant event-source |
| D3 | JSON — свой hand-rolled (ориентир `jsonc-parser` API) | нужен byte round-trip + spans; serde_json их не даёт |
| D4 | JSON5/JSONC — свой парсер-расширение JSON | trivia-preserving JSON5 в Rust отсутствует |
| D5 | `.env`/INI/Properties — свои line-парсеры | существующие дропают trivia или заброшены; грамматики тривиальны |
| D6 | HCL — `hcl-edit`/`hcl-rs`, scope «упрощённый» | единственный trivia-HCL; expressions → opaque + warning |
| D7 | KDL — `kdl` (kdl-rs) | единственный trivia-KDL («toml_edit для KDL») |
| D8 | RON — `ron` + свой comment-lex; round-trip partial | `ron2` незрел; честно фиксируем ограничение |

## 4. Риски и что не получилось выяснить на Этапе 0

1. **YAML trivia-collector — главный риск.** Точная привязка комментариев
   к узлам (head/line/foot), indent- edge cases, flow vs block, multi-doc
   headers — потребует итераций на Этапе 3. Митигация: subset-first
   (mappings/lists/scalars/comments/multiline/anchors), остальное —
   explicit `UnsupportedConstruct` + warning, а не молчание.
2. **WASM (Этап 7):** `toml_edit`, `saphyr-parser`, `kdl`, `hcl-edit`
   — pure Rust, должны собраться под `wasm32-unknown-unknown`; `ron`,
   свой код — тоже. Риск низкий, но проверить на Этапе 1 через
   `cargo check --target wasm32-unknown-unknown` для core.
3. **Лицензии:** всё выбранное — MIT OR Apache-2.0 (проверено по
   crates.io/docs.rs). `taplo` тоже MIT, но не выбран. GPL-зависимостей
   не тянем — требование «MIT OR Apache-2.0» выполнимо.
4. **5 МБ < 1 c:** на Этапе 0 не замерялось (кода нет). Все выбранные
   парсеры — линейные/предиктивные; собственный JSON/YAML спроектируем
   single-pass. Проверка — bench на Этапе 2.

## 5. План Этапа 1 (задел, код — только со следующего этапа)

Workspace + CI (fmt/clippy/test × linux/mac/windows) + IR + `trait Format`
+ `Warning` + CLI `convert <in> [-f FROM] -t TO [-o OUT] [--strict]`
(+ определение по расширению, stdin), golden-каркас
`tests/fixtures/*`, `cargo-fuzz`/`criterion` заготовки, `wasm`-check.
Первый формат (JSON-stub) — только чтобы прогнать pipeline, без претензий
на round-trip (это Этап 2).
