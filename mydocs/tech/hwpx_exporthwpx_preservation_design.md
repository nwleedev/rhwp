# HWPX `exportHwpx()` 보존 설계 기록

## 목적

이 문서는 `fix/hwpx-section-format-roundtrip` 브랜치에서 진행한 HWPX `exportHwpx()` 보존 작업을 rhwp 저장소 관점으로 정리한다. 목표는 HWPX를 로드한 뒤 `exportHwpx()`로 다시 저장했을 때 원본 문서의 package 구조, section graph, page formatting, object/table/control reference, 사용자에게 보이는 layout이 최대한 유지되는지 검증하고, 손실 원인을 rhwp parser/model/serializer/studio 계층에서 줄이는 것이다.

이 브랜치는 이슈 번호가 확정되지 않은 fork 작업이다. 원본 저장소에 유사 이슈가 존재하므로 새 GitHub Issue 번호를 임의로 만들지 않는다. 따라서 `mydocs/plans/task_m100_{issue}.md`, `mydocs/working/task_m100_{issue}_stage{N}.md`, `mydocs/report/task_m100_{issue}_report.md`, `mydocs/orders/YYYYMMDD.md` 체계는 아직 사용하지 않고, 이 문서와 troubleshooting 문서가 현재 인수인계의 source of truth 역할을 한다.

## 읽는 순서

번호 없는 상태에서 작업을 이어받는 사람은 다음 순서로 확인한다.

1. 이 문서의 "완료로 간주하는 현재 보존 시나리오"와 "백로그로 분리한 항목"을 먼저 읽는다.
2. "커밋 이력 지도"에서 현재 브랜치가 어떤 문제를 어떤 순서로 줄였는지 확인한다.
3. `mydocs/troubleshootings/hwpx_export_roundtrip_preservation.md`에서 같은 문제가 다시 생겼을 때 어떤 가정을 금지해야 하는지 확인한다.
4. 새 issue 번호가 확정되면 이 문서를 근거로 `mydocs/plans/`, `mydocs/working/`, `mydocs/report/`, `mydocs/orders/` 문서를 생성한다.

## 완료로 간주하는 현재 보존 시나리오

현재 단계에서는 다음 시나리오가 충족된 것으로 본다.

- no-edit HWPX export에서 원본 package entry set과 entry별 byte hash가 유지된다.
- `Contents/content.hpf` manifest/spine에서 온 section href와 section order가 보존된다.
- 비표준 section href와 multi-section spine order를 파일명 패턴으로 재생성하지 않는다.
- no-edit raw XML 보존 여부가 event log 공백 여부에 종속되지 않고 document mutation state와 분리된다.
- nested table, borderFill reference, note/control/table/object 관련 보존 회귀를 current branch의 serializer/model/studio proof로 추적한다.
- headed browser verification에서 export 전 preview와 export 후 reload preview가 page count, layout fingerprint, rendered visual verdict 기준으로 통과한다.

## 백로그로 분리한 항목

다음 항목은 보존성 주장을 더 강하게 만드는 후속 검증이지만, 현재 브랜치를 커밋 가능한 상태로 정리하는 작업을 차단하지 않는다.

- `fieldEmptyGuideTransition` 실제 fixture 확보: empty guide field 전이를 가진 공개 또는 승인 가능한 HWPX 샘플 확보가 필요하다.
- local PC/Hancom 확인 evidence: desktop Hancom 또는 로컬 PC viewer에서 사람이 확인하거나 자동화 가능한 검증 경로가 필요하다.

이 두 항목은 `backlog` verdict로 추적한다. 현재 browser/runtime 기반 보존 시나리오를 실패로 되돌리는 조건으로 쓰지 않는다.

## 설계 원칙

- 특정 테스트 파일 이름, checksum, XML tag count, control id, known ordering에 의존하지 않는다.
- HWPX package 구조는 `Contents/content.hpf` manifest와 spine을 우선 source of truth로 삼는다.
- serializer가 section entry 이름을 `Contents/section{N}.xml`로 추정해 원본 href를 잃지 않도록 한다.
- no-edit export와 edited export를 같은 조건으로 판단하지 않는다.
- event log, capture proof, command history는 편집 관찰 evidence이지 no-edit 판단의 단독 source of truth가 아니다.
- 시각 동일성은 package/XML 동일성을 증명하지 않고, package/XML 동일성은 사용자 가시 layout 동일성을 단독으로 증명하지 않는다.

## 보존 계층

`exportHwpx()` 보존 여부는 한 가지 pass/fail로 합치지 않는다.

| 계층 | 확인할 것 | 실패 예시 | 현재 브랜치의 대응 |
| --- | --- | --- | --- |
| Package graph | ZIP entry set, `mimetype`, `Contents/content.hpf`, manifest/spine, section href, BinData/object reference | section entry를 `Contents/section{N}.xml`로 재생성해 원본 href를 잃음 | `content.hpf` manifest/spine 기반 section order와 href 보존 |
| Raw XML reuse | no-edit 문서에서 원본 XML subtree/entry를 재사용할 수 있는지 | event log가 비어 있다는 이유만으로 stale original XML을 export | mutation state와 event/capture evidence 분리 |
| Serializer coverage | page margin, page border/fill, header/footer, style, note, field, object, table 속성 | template 기본값이 원본 page setting을 덮어씀 | page section margin, pageBorderFill, metadata, nested table borderFill 참조 보강 |
| Runtime proof | studio message API로 runtime identity와 layout fingerprint를 수집할 수 있는지 | package version만 보고 다른 build를 검증했다고 착각 | runtime identity RPC와 formatting fingerprint RPC 추가 |
| Edit guardrail | table/note/field/object/page mutation이 지원 또는 unsupported로 분류되는지 | 복잡한 편집을 replay 가능하다고 잘못 승격 | proof RPC와 unsupported capture category로 분리 |
| Visual evidence | export 전 preview와 export 후 reload의 page count/layout/rendered output | 구조가 맞아도 사용자가 보는 페이지가 달라짐 | headed browser 기반 visual/layout/page verdict 기록 |
| Desktop oracle | Hancom 또는 local PC viewer에서 열림/시각 확인 | rhwp 자기 정합만으로 desktop 호환을 주장 | 현재는 backlog verdict로 분리 |

## 브랜치 커밋 이력에서 확인되는 작업 축

`fix/hwpx-section-format-roundtrip` 브랜치의 최근 커밋은 다음 흐름으로 정리된다.

- `exportHwpx` 원본 package 구조 보존 일반화
- `content.hpf` manifest/spine 섹션 순서와 fallback 판정 보강
- no-edit export 문서 XML 보존
- export package metadata 보존
- nested table borderFill reference 수집 보강
- control paste, picture/shape mutation, table/note/field 관련 studio proof RPC와 unsupported guardrail 보강
- header/footer page field, line shape, 구조 control 앞 텍스트 삽입 위치 등 serializer/materialization 회귀 보강

이 흐름은 browser verification 통과만을 목표로 한 것이 아니라, rhwp 내부의 parser/model/serializer/studio message boundary가 arbitrary HWPX 구조를 더 잘 보존하도록 만드는 작업이다.

## 커밋 이력 지도

현재 브랜치의 커밋은 크게 여섯 묶음으로 읽는다. 새 작업자는 개별 커밋을 cherry-pick하거나 되돌리기 전에 같은 묶음 안의 선후관계를 확인해야 한다.

### 1. 원본 section/page formatting 보존 기반

| 커밋 | 역할 |
| --- | --- |
| `6aa16aaa` | page section margin 보존. template 기본값이 원본 page setting을 덮는 문제의 시작점 수정. |
| `d15224e3` | header/footer id 보존. section과 header/footer reference가 재생성되며 달라지는 위험 완화. |
| `26e0de79` | document paraPr switch preservation gap 기록. 이후 보존 대상 누락을 테스트로 드러내기 위한 gap 기록. |
| `4f45aab6` | header switch branch 보존. header 내부 조건부 구조가 serializer에서 사라지는 위험 완화. |
| `6d2f28cb` | table cell paragraph 구조 보존. table 내부 paragraph topology 손실 완화. |
| `e72a9a4a` | run segmentation 보존 회귀 기록. text/control run 경계 보존 작업의 회귀 기준. |
| `39cdd1f5` | char shape run segment 보존. 글자 모양 구간 경계가 합쳐지는 문제 완화. |
| `cd23b101` | table caption subList 보존. caption 내부 list/paragraph 구조 손실 완화. |
| `b85d7d39` | page border fill type 보존 gap 기록. page border/fill 계열의 남은 위험을 명시. |
| `6d230d5c` | page border fill 적용 대상 보존. page border/fill semantics를 template default가 덮지 않도록 보강. |

### 2. run/control/XML subtree 보존

| 커밋 | 역할 |
| --- | --- |
| `d0fc2986` | text run 경계 보존. 단순 text 재직렬화가 run boundary를 잃지 않도록 함. |
| `b077aaf5` | inline object run 경계 보존. inline object 앞뒤 run 구조 보존. |
| `53d486cd` | column control run 경계 보존. column control 주변 run topology 보존. |
| `4a17544d` | field run 경계 보존. field control 경계 손실 완화. |
| `f10d622c` | auto number run 경계 보존. 자동 번호 control 주변 run 경계 보존. |
| `7122923d` | bookmark run 경계 보존. bookmark marker만 있는 구간 보존. |
| `b4954f0d` | first paragraph section run 경계 보존. section 첫 문단 특수 경계 보존. |
| `349de789` | bookmark-only run 경계 보존. 내용 없는 bookmark run 손실 방지. |
| `024b283c` | empty text run fragment 보존. 빈 text fragment가 serializer에서 사라지는 문제 완화. |
| `2fabe6ea` | field parameters subtree 보존. field metadata XML subtree 보존. |
| `ad6563c1` | inline object structure 보존. object subtree 재직렬화 손실 완화. |

### 3. page/layout/object metadata 보존과 runtime identity

| 커밋 | 역할 |
| --- | --- |
| `0e9c1a53` | runtime identity RPC 추가. 검증 대상 build/source를 host가 식별할 수 있게 함. |
| `444b4d4a` | formatting fingerprint RPC 추가. page/layout fingerprint를 수집할 수 있게 함. |
| `182d3bcd` | startNum과 border width roundtrip 보존. numbering과 border width drift 완화. |
| `5e680cae` | table pageBreak 원본 속성 보존. table pagination 관련 원본 속성 손실 완화. |
| `0cc98371` | markpen 범위 경계 보존. markpen range boundary 손실 완화. |
| `b0829c26` | zero-width 경계와 첫 문단 lineseg 보존. 빈 경계와 line segment drift 완화. |
| `73421d0b` | 문단 레이아웃 속성 보존. paragraph layout metadata drift 완화. |
| `21f2fc9c` | 글자 그림자와 주석 모양 보존. char shadow/comment shape metadata 보존. |
| `3048456d` | header style table 보존. header style table 손실 방지. |
| `9aa9e3ee` | memo와 subList 서식 보존. memo/subList formatting drift 완화. |
| `98fed70b` | object `flowWithText` 보존. object text-flow semantics 손실 완화. |
| `8837f675` | bullet 정의 보존. bullet definition 손실 완화. |

### 4. deterministic edit, capture coverage, supported/unsupported proof

| 커밋 | 역할 |
| --- | --- |
| `4eebb8bf` | deterministic edit RPC 추가. 반복 가능한 편집 proof 진입점 제공. |
| `1d03e050` | deterministic edit 기본 offset 안정화. proof 위치 선택의 흔들림 완화. |
| `6eeaaee8` | deterministic edit를 input handler 경로로 적용. 실제 편집 경로에 더 가까운 proof로 이동. |
| `fb6a0f7d` | capture coverage RPC 추가. 어떤 mutation surface가 관찰됐는지 수집. |
| `56eac30e` | direct mutation capture coverage 추가. command dispatcher 밖 mutation 감지. |
| `25d2f053` | field value capture RPC 추가. field value 변경 proof. |
| `c8fd55b6` | field id value capture RPC 추가. id 기반 field value proof. |
| `ff520f46` | table cell text proof RPC 추가. table cell text 변경 proof. |
| `7ee94a1b` | unsupported snapshot capture 추가. replay 불가 mutation을 unsupported로 남김. |
| `c0b3486c` | table resize proof RPC 추가. table geometry mutation guardrail. |
| `8e078d96` | footnote text proof RPC 추가. footnote text edit proof. |
| `b97e82c5` | footnote target discovery 보정. note target 탐색 안정화. |
| `95360a0e` | 각주 HWPX 보존 보강. note 계층 보존 확장. |

### 5. field/note/object/paste proof와 visual fingerprint 보정

| 커밋 | 역할 |
| --- | --- |
| `e29b823c` | HWPX header roundtrip layout metadata 보존. header layout drift 완화. |
| `7073a4f9` | visual fingerprint에서 layer tree node count 제외. 의미 없는 node count drift로 visual proof가 흔들리는 문제 완화. |
| `96123c5e` | field metadata capture proof RPC 추가. field metadata mutation 관찰. |
| `e58e195d` | HWPX field metadata proof capture 추가. metadata proof와 HWPX 경로 연결. |
| `bc21fc11` | field delete proof RPC 추가. field deletion guardrail. |
| `67b790bb` | field delete proof 범위 보정. 삭제 proof 대상 안정화. |
| `13193fb4` | field create proof RPC 추가. field creation guardrail. |
| `36e6713c` | header/footer page field HWPX materialization. header/footer page field 생성 materialization 보강. |
| `eac24228` | undo/redo capture proof RPC 추가. history/coalescing 관찰. |
| `086183f5` | record-only capture proof RPC 추가. no-op record path 관찰. |
| `a8b8fe80` | IME composition capture proof RPC 추가. composition input mutation 관찰. |
| `cc715026` | iOS fallback input capture proof RPC 추가. platform fallback input mutation 관찰. |
| `64959244` | image paste unsupported capture proof RPC 추가. image paste를 unsupported guardrail로 분류. |
| `a40bd947` | image paste proof category 분류 보정. paste category 안정화. |
| `a40e20cc` | internal paste unsupported capture proof RPC 추가. internal paste guardrail. |
| `e339ae39` | internal paste proof target 탐색 보정. paste target 안정화. |
| `bf410b2f` | control paste unsupported capture proof RPC 추가. control paste guardrail. |
| `92458c02` | control paste pagination roundtrip 보존. control paste 후 pagination drift 완화. |
| `d0e90503` | picture object mutation proof RPC 추가. picture object mutation guardrail. |
| `f51faaa2` | shape object mutation proof RPC 추가. shape mutation guardrail. |
| `5192438e`, `2b716195`, `cb46cc22`, `fe73006d` | shape proof seed와 line shape 좌표 보존 안정화. |

### 6. package graph, manifest/spine, no-edit preservation 완성 축

| 커밋 | 역할 |
| --- | --- |
| `ffef8162` | note topology capture proof. note topology mutation 구분. |
| `63453b1e`, `48e76a99` | note insertion metadata insert guard와 char offset clamp. |
| `ac81419f`, `17fa281f` | table span/nested table target metadata 노출. |
| `46aeb316` | 구조 control 앞 텍스트 삽입 위치 보존. control-adjacent text edit 위치 안정화. |
| `3af1c27c` | export package metadata 보존. package-level metadata drift 완화. |
| `09fe5a0c` | content manifest 보존 조건 정규화. manifest preservation 분기 안정화. |
| `c77bef01` | no-edit export 문서 XML 보존. no-edit raw XML reuse 축 강화. |
| `d5161693` | `exportHwpx` 원본 package 구조 보존 일반화. 현재 private file 전용이 아닌 package graph 기반으로 일반화. |
| `a1f4b735` | manifest spine 순서 보존 검증. section order proof 추가. |
| `cf68654c` | `content.hpf` section fallback 판정 보강. spine 부재/불완전 상황 fallback의 filename-substring risk 완화. |
| `76bbbd1b` | nested table borderFill 참조 수집 보강. nested table reference graph 보존 강화. |

## 검증 기록 원칙

검증 결과는 다음 축을 분리해서 기록한다.

- package entry set, size, SHA-256, missing/added/changed entry
- XML semantic/topology fingerprint
- manifest/spine/reference graph 보존 여부
- runtime identity: source commit, patch id, build id
- page count, layout fingerprint, rendered visual verdict
- browser download byte/SHA 일치 여부
- local PC/Hancom verdict 또는 backlog 상태

raw HWPX, raw XML, full SVG, screenshot, trace, 다운로드 파일은 커밋하지 않는다. 장기 보존 문서는 raw-safe summary와 재실행 명령, runtime identity, verdict만 남긴다.

## issue 번호가 생긴 뒤 생성할 문서

현재는 issue 번호를 임의 생성하지 않으므로 `plans/working/report/orders`를 만들지 않는다. 나중에 issue 번호가 확정되면 다음 mapping으로 이 문서를 분해한다.

| 문서 | 내용 |
| --- | --- |
| `mydocs/plans/task_m100_{issue}.md` | `exportHwpx()` 보존 문제, 완료 시나리오, 백로그, non-goal. |
| `mydocs/plans/task_m100_{issue}_impl.md` | parser/model/serializer/studio proof/build verification 단계. |
| `mydocs/working/task_m100_{issue}_stage1.md` | section/page formatting과 run/control boundary 보존 단계. |
| `mydocs/working/task_m100_{issue}_stage2.md` | runtime identity, formatting fingerprint, deterministic edit/capture proof 단계. |
| `mydocs/working/task_m100_{issue}_stage3.md` | package graph, manifest/spine, dirty state, no-edit raw XML preservation 단계. |
| `mydocs/report/task_m100_{issue}_report.md` | 현재 완료 verdict, 백로그 verdict, 검증 명령과 남은 위험. |
| `mydocs/orders/YYYYMMDD.md` | 해당 issue 작업 상태와 다음 action. |

## 다음 작업자가 바로 확인할 질문

- 이 변경이 `Contents/content.hpf` manifest/spine의 section href와 order를 보존하는가?
- no-edit 여부를 event log 공백만으로 판단하지 않는가?
- 새 serializer fallback이 특정 fixture filename, tag count, XML ordering, id pattern에 의존하지 않는가?
- edited document에서 stale original XML을 export할 수 있는 경로가 없는가?
- visual/layout/page verdict와 package/XML verdict를 섞어 쓰지 않았는가?
- local PC/Hancom 확인이 없다면 pass가 아니라 backlog 또는 unverified로 남겼는가?

## 커밋 전 확인

- `mydocs/`에 필요한 설계/기술 기록이 남아 있어야 한다.
- `AGENTS.md`와 `CLAUDE.md`의 문서 규칙이 충돌하지 않아야 한다.
- `exportHwpx()` 보존 관련 변경은 하드코딩 또는 fixture-specific assumption으로 통과하지 않았음을 설명할 수 있어야 한다.
- 백로그 항목은 실패 gate가 아니라 후속 검증으로 명확히 분류되어야 한다.
