# HWPX `exportHwpx()` 보존 설계 기록

## 목적

이 문서는 `fix/hwpx-section-format-roundtrip` 브랜치에서 진행한 HWPX `exportHwpx()` 보존 작업을 rhwp 저장소 관점으로 정리한다. 목표는 HWPX를 로드한 뒤 `exportHwpx()`로 다시 저장했을 때 원본 문서의 package 구조, section graph, page formatting, object/table/control reference, 사용자에게 보이는 layout이 최대한 유지되는지 검증하고, 손실 원인을 rhwp parser/model/serializer/studio 계층에서 줄이는 것이다.

## 완료로 간주하는 현재 보존 시나리오

현재 단계에서는 다음 시나리오가 충족된 것으로 본다.

- no-edit HWPX export에서 원본 package entry set과 entry별 byte hash가 유지된다.
- `Contents/content.hpf` manifest/spine에서 온 section href와 section order가 보존된다.
- 비표준 section href와 multi-section spine order를 파일명 패턴으로 재생성하지 않는다.
- no-edit raw XML 보존 여부가 event log 공백 여부에 종속되지 않고 document mutation state와 분리된다.
- nested table, borderFill reference, note/control/table/object 관련 보존 회귀를 current branch의 serializer/model/studio proof로 추적한다.
- headed browser harness에서 export 전 preview와 export 후 reload preview가 page count, layout fingerprint, rendered visual verdict 기준으로 통과한다.

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

## 브랜치 커밋 이력에서 확인되는 작업 축

`fix/hwpx-section-format-roundtrip` 브랜치의 최근 커밋은 다음 흐름으로 정리된다.

- `exportHwpx` 원본 package 구조 보존 일반화
- `content.hpf` manifest/spine 섹션 순서와 fallback 판정 보강
- no-edit export 문서 XML 보존
- export package metadata 보존
- nested table borderFill reference 수집 보강
- control paste, picture/shape mutation, table/note/field 관련 studio proof RPC와 unsupported guardrail 보강
- header/footer page field, line shape, 구조 control 앞 텍스트 삽입 위치 등 serializer/materialization 회귀 보강

이 흐름은 browser harness 통과만을 목표로 한 것이 아니라, rhwp 내부의 parser/model/serializer/studio message boundary가 arbitrary HWPX 구조를 더 잘 보존하도록 만드는 작업이다.

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

## 커밋 전 확인

- `docs/designs`가 아니라 `mydocs/`에 필요한 설계/기술 기록이 남아 있어야 한다.
- `AGENTS.md`와 `CLAUDE.md`의 문서 규칙이 충돌하지 않아야 한다.
- `exportHwpx()` 보존 관련 변경은 하드코딩 또는 fixture-specific assumption으로 통과하지 않았음을 설명할 수 있어야 한다.
- 백로그 항목은 실패 gate가 아니라 후속 검증으로 명확히 분류되어야 한다.
