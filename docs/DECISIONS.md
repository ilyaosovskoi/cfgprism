# docs/DECISIONS.md — журнал решений (append-only)

Формат записи: `## Dn. <заголовок> — <дата>` + Контекст / Решение / Обоснование / Альтернативы.
Неоднозначности ТЗ решаются здесь, а не в чате.

## D1. TOML — `toml_edit`, не `taplo` — 2026-09-30

- Контекст: нужны trivia (комменты/пробелы/порядок) + byte round-trip + line:col.
- Решение: `toml_edit` (toml-rs, spec-1.1.0).
- Обоснование: `DocumentMut::to_string()` даёт round-trip из коробки;
  `decor`/`repr` прямо маппятся на IR Trivia/Style; лёгкие зависимости
  (важно для статического бинарника и WASM).
- Альтернативы: `taplo` (rowan green-tree, полный fidelity + форматтер/LSP,
  но тяжёлый dependency-tree ~160K SLoC транзитивно) — отклонён как оверхед.
  Известные потери `toml_edit` (dotted-keys порядок, reorder scattered tables)
  покрываем warning-ами и тестами, а не замалчиваем.

## D2. YAML — `saphyr-parser` + свой trivia-collector — 2026-09-30

- Контекст: ТЗ требует YAML round-trip без потерь + якоря/алиасы + комменты.
- Решение: event-source `saphyr-parser` (YAML 1.2 compliant, активен;
  `yaml-rust2` — только maintenance) + собственный сканер trivia
  (комментарии/пустые строки/стили/кавычки/block/flow) с аттачем по строкам.
- Обоснование: готового trivia-preserving YAML-крейта в Rust нет
  (проверены `yaml-rust2`, `saphyr`, `serde-saphyr`/`granit-parser`:
  либо дропают комменты, либо ловят только `Commented<T>`, freestanding —
  нет). Писать YAML-парсер с нуля дороже, чем коллектор поверх compliant events.
- Альтернативы: `yaml-rust2` (тот же недостаток + хуже compliance),
  `unsafe-libyaml` (C-зависимость, ломает статик-билд и WASM) — отклонены.

## D3. JSON (strict) — свой hand-rolled парсер — 2026-09-30

- Контекст: нужен byte round-trip JSON в себя + spans + сохранение порядка.
- Решение: собственный recursive-descent (ориентир API — `jsonc-parser`
  с `comments/tokens`), финализация бенчмарком на Этапе 2.
- Обоснование: `serde_json` дропает комментарии/whitespace (порядок только
  с `preserve_order`); `jsonc-parser` ближе всего, но тянем свой для контроля
  round-trip без surprises CST-API.
- Альтернативы: напрямую `jsonc-parser` — запасной вариант, если свой
  проиграет по fuzz/bench.

## D4. JSONC/JSON5 — свой парсер-расширение JSON — 2026-09-30

- Контекст: JSONC/JSON5 — надмножества JSON (комменты, trailing commas,
  unquoted keys, single-quotes, hex, multiline).
- Решение: один парсер с флагом диалекта, trivia из D3 + расширения.
- Обоснование: trivia-preserving JSON5-крейта в Rust нет
  (`json5-rs`/`serde_json5` — serde-only, дропают trivia;
  `json5format` — только форматтер). Грамматика мала — писать самим.
- Альтернативы: форк `json5format` — отклонён (не парсер под IR).

## D5. `.env` / INI / Properties — свои line-парсеры — 2026-09-30

- Контекст: ТЗ требует byte round-trip простых line-форматов.
- Решение: три маленьких собственных парсера (~200 строк каждый).
- Обоснование: `dotenv/dotenvy` — loader'ы (теряют комменты/кавычки/`export`);
  `dotenv-parser` заброшен 5+ лет; `rust-ini/configparser` не гарантируют
  round-trip. Грамматики тривиальны — дешевле написать, чем чинить чужое.

## D6. HCL — `hcl-edit`/`hcl-rs`, scope «упрощённый HCL» — 2026-09-30

- Контекст: Этап 5 требует HCL; полный HCL включает expressions/templates/eval.
- Решение: парсинг/round-trip через `hcl-edit` («toml_edit для HCL»);
  значения через `hcl-rs`; scope — атрибуты/блоки/литералы; `for`/functions/
  `${}` маппятся в IR как opaque-скаляры + `WarningKind::UnsupportedConstruct`.
- Обоснование: единственный trivia-preserving HCL в экосистеме; полный eval —
  отдельный проект, вне scope конвертера.
- Альтернативы: писать HCL-парсер самим — отклонено (дорого, хуже compliance
  с go-hcl).

## D7. KDL — `kdl` (kdl-rs v2) — 2026-09-30

- Контекст: нужен trivia-KDL.
- Решение: крейт `kdl` (document-oriented, хранит formatting/comments,
  byte round-trip, v1/v2).
- Обоснование: дословный аналог `toml_edit` для KDL; `knus` отклонён —
  он serde-derive и дропает trivia.

## D8. RON — `ron` + свой comment-lex; round-trip только partial — 2026-09-30

- Контекст: RON нужен на Этапе 5, но trivia-RON в Rust отсутствует.
- Решение: значения через `ron`, комментарии — собственным pre-lex с аттачем
  к IR; byte round-trip для RON НЕ обещаем, в README-матрице будет
  `round-trip: partial` + warning при нормализации.
- Обоснование: `ron2` (AST-based, 2026) незрел (единицы звёзд, unstable API).
  Честное ограничение лучше выдуманного round-trip.
- Альтернативы: ждать/мигрировать на `ron2` при стабилизации — зафиксировано
  как follow-up.
