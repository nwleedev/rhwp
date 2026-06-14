# HWPX `exportHwpx()` 라운드트립 보존 트러블슈팅

## 목적

이 문서는 HWPX를 로드한 뒤 `exportHwpx()`로 다시 저장했을 때 구조 또는 시각 결과가 달라지는 문제를 다시 만났을 때 확인할 원인과 금지할 가정을 정리한다. 현재 브랜치의 기술 설계 요약은 `mydocs/tech/hwpx_exporthwpx_preservation_design.md`에 있고, 이 문서는 재발 방지용 진단 절차를 담당한다.

## 대표 증상

- HWPX를 열고 저장만 했는데 ZIP entry 이름, entry 수, entry hash가 달라진다.
- `Contents/content.hpf`의 manifest/spine section href가 serializer 이후 `Contents/section{N}.xml` 같은 새 이름으로 바뀐다.
- page margin, page border/fill, header/footer, background, page number, object reference가 원본과 달라진다.
- rhwp-studio에서 export 후 reload한 결과가 export 전 preview와 page count 또는 layout fingerprint 기준으로 다르다.
- package/XML은 비슷해 보이지만 desktop Hancom 또는 local viewer에서 보이는 결과를 아직 확인하지 못했다.

## 주요 원인과 대응

| 원인 | 잘못된 가정 | 올바른 대응 |
| --- | --- | --- |
| section href 재생성 | section entry는 항상 `Contents/section0.xml`, `Contents/section1.xml` 순서다. | `Contents/content.hpf` manifest/spine에서 href와 order를 읽어 보존한다. |
| template default overwrite | 빈 section template의 margin/pageBorderFill을 그대로 써도 된다. | 원본 section page setting을 parser/model/serializer에 싣고, 없을 때만 명시 fallback을 쓴다. |
| event log 기반 no-edit 판단 | `event_log.is_empty()`이면 문서가 수정되지 않았다. | event/capture evidence와 semantic mutation state를 분리한다. |
| raw XML stale export | capture event가 비었으니 original XML을 재사용해도 된다. | edited document는 stale original XML로 export되지 않도록 mutation state를 확인한다. |
| fixture-specific pass | 현재 fixture의 filename, tag count, id pattern, XML ordering이 일반 규칙이다. | package graph, parser/model IR, 표준 기반 default에서만 값을 가져온다. |
| visual-only pass | reload 화면이 비슷하면 package/XML도 보존됐다. | package entry, XML topology, layout fingerprint, rendered visual verdict를 분리한다. |
| self-roundtrip pass | rhwp에서 열리고 다시 열리면 Hancom 호환도 통과다. | desktop Hancom/local PC evidence가 없으면 별도 backlog 또는 unverified verdict로 둔다. |

## 진단 순서

1. 원본 HWPX와 export HWPX의 ZIP entry set, entry size, entry hash를 비교한다.
2. `Contents/content.hpf` manifest item과 spine order를 비교한다.
3. `Contents/section*.xml`, header/footer, BinData reference graph의 missing/added/changed entry를 raw-safe summary로 기록한다.
4. no-edit 상태라면 mutation state와 raw XML reuse 조건을 별도로 확인한다.
5. edited 상태라면 어떤 mutation이 supported candidate인지 unsupported guardrail인지 먼저 분류한다.
6. runtime identity와 formatting fingerprint를 확인해 다른 build를 검증하지 않았는지 확인한다.
7. rendered visual evidence는 page count, layout fingerprint, rendered output을 분리해 판정한다.
8. desktop Hancom/local PC 확인이 없다면 pass로 승격하지 않고 backlog 또는 unverified로 남긴다.

## 보존 verdict 용어

| Verdict | 의미 |
| --- | --- |
| `package_graph_preserved` | ZIP entry와 manifest/spine/reference graph가 보존됨. |
| `xml_topology_preserved` | XML tag topology와 보존 대상 attribute/subtree가 의미상 보존됨. |
| `layout_fingerprint_match` | runtime layout fingerprint가 일치함. |
| `visual_match` | rendered output이 비교 기준을 통과함. package/XML 보존을 의미하지 않음. |
| `runtime_identity_match` | 검증 runtime의 source/build identity가 대상 커밋과 일치함. |
| `unsupported_mutation_guarded` | mutation을 replay/materialize하지 않고 unsupported로 안전하게 분류함. |
| `local_pc_unverified` | desktop Hancom/local PC 확인이 아직 없음. |
| `backlog` | 현재 완료 조건을 막지 않는 후속 검증 항목. |

## 현재 백로그

- empty guide field 전이를 가진 실제 fixture 확보.
- desktop Hancom 또는 local PC viewer에서의 수동/자동 시각 확인.

이 둘은 보존성을 더 강하게 만드는 후속 검증이다. 현재 browser/runtime 기반 보존 시나리오가 이미 통과한 상태에서는 실패 gate가 아니라 backlog verdict로 둔다.

## 재발 방지 체크리스트

- [ ] 새 serializer fallback이 `content.hpf` manifest/spine보다 파일명 패턴을 우선하지 않는다.
- [ ] no-edit 판단은 event log 공백이 아니라 semantic mutation state를 사용한다.
- [ ] edited document export가 stale original XML을 반환할 수 없다.
- [ ] table/note/field/object/page mutation은 supported candidate 또는 unsupported guardrail 중 하나로 기록된다.
- [ ] proof RPC는 runtime identity와 함께 기록된다.
- [ ] 시각 evidence와 package/XML evidence를 같은 verdict로 합치지 않는다.
- [ ] raw HWPX, raw XML, screenshot, SVG, browser trace, 다운로드 파일을 커밋하지 않는다.
