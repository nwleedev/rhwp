# AGENTS.md

이 문서는 Codex 같은 코드 에이전트가 이 저장소에서 작업할 때 따를 규칙이다. 기존 `CLAUDE.md`를 대체하지 않고, Codex 작업 시 우선 읽는 보조 지침으로 사용한다.

## 언어와 문서

- 사용자와의 설명, 설계 문서, 작업 보고서는 한국어로 작성한다.
- 코드, 파일명, 모듈명, public API 이름은 기존 저장소 관례를 따른다.
- 요구사항, 계획, 기술 조사, 결과 보고는 `docs/designs`가 아니라 `mydocs/` 아래에 작성한다.
- `mydocs/`의 폴더 역할은 `CLAUDE.md`의 문서 생성 규칙을 따른다.

## 작업 절차

- 소스 수정 전에는 현재 브랜치와 작업 트리 상태를 확인한다.
- 사용자가 구현을 명시하지 않았으면 코드 수정 대신 조사, 설계, 리뷰, 계획까지만 진행한다.
- 기존 변경사항을 임의로 되돌리지 않는다.
- destructive git 명령은 사용자 승인 없이 실행하지 않는다.
- 검색은 먼저 `rg` 또는 `rg --files`를 사용한다.
- 커밋 전에는 민감정보, 개인 경로, private 문서 원문, 테스트용 원본 HWPX 내용이 포함되지 않았는지 확인한다.

## rhwp 구조

- HWPX 파서는 `src/parser/hwpx/`의 책임을 우선 확인한다.
- HWP5/HWP3 전용 로직을 HWPX 수정에 섞지 않는다.
- 공통 문서 모델은 `src/model/document.rs`의 `Document` IR을 기준으로 한다.
- 렌더러, 레이아웃, serializer 수정은 parser/model에서 실제 의미가 보존되는지 확인한 뒤 최소 범위로 적용한다.

## HWPX export 보존 작업 규칙

- `exportHwpx()` 또는 HWPX 저장 경로를 수정할 때 특정 fixture 파일명, checksum, tag count, XML 순서, control id, known value에 의존해 통과시키지 않는다.
- 보존 대상 값은 HWPX package manifest, parser/model IR, 원본 package entry, 표준 근거가 있는 deterministic default, 또는 명시적인 unsupported fallback에서 가져온다.
- `Contents/content.hpf`의 manifest/spine, section href, section order, binary/object reference graph를 임의 파일명 패턴으로 재구성하지 않는다.
- no-edit raw XML 보존 여부는 이벤트 로그가 비어 있는지가 아니라 문서 semantic mutation state로 판단한다.
- 시각 보존, page count, layout fingerprint, ZIP entry/hash, XML semantic/topology 보존은 서로 다른 verdict로 기록한다.
- 브라우저에서 시각 확인을 수행할 때는 가능한 headed 모드 evidence를 남긴다.

## 검증

- Rust 변경 후에는 관련 단위 테스트 또는 통합 테스트를 우선 실행한다.
- WASM/studio 변경 후에는 source commit, build id, patch id, asset checksum, 재빌드 절차를 문서화한다.
- 외부 harness에서 확인한 결과는 raw 문서 내용을 커밋하지 않고, raw-safe summary만 `mydocs/`에 남긴다.
- raw HWPX, raw XML, screenshot, SVG, browser trace, 다운로드 산출물은 Git에 커밋하지 않는다.

## 커밋

- 커밋은 작업 단위가 분리될 때 수행한다.
- 제목은 기존 저장소 스타일을 따른다. 예: `fix(hwpx): ...`, `test(studio): ...`
- 본문에는 왜 변경했는지와 어떤 보존/검증 gate를 만족하는지 적는다.
