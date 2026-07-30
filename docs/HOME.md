---
tags:
  - firecrab
  - moc
updated: 2026-07-30
---

# firecrab 문서

> [!summary] 이 볼트는 무엇인가
> [firecrab](../README.md)은 AWS Firecracker 위에 올린 **자체 호스팅 microVM 클라우드**다.
> 이 볼트에는 그 설계·계획·검증 기록이 들어 있다.
> 코드 주석과 커밋은 영어, 문서 본문은 한국어다.

## 어디서 시작하나

| 하려는 일 | 문서 |
|---|---|
| 무엇인지 알고 싶다 | [아키텍처](10-overview/architecture.md) · [AWS 대응표](10-overview/aws-mapping.md) |
| 용어가 낯설다 | [용어집](10-overview/glossary.md) |
| 직접 띄워보고 싶다 | [설치](20-guides/install.md) → [대시보드](20-guides/web.md) |
| API를 쓰고 싶다 | [API](20-guides/api.md) · [오류 계약](20-guides/api-error.md) |
| 안 되는 게 있다 | [트러블슈팅](20-guides/troubleshooting.md) → [버그 기록](50-bugs/MOC-bugs.md) |
| 지금 뭘 하고 있나 | [태스크](30-tasks/MOC-tasks.md) · [4주차](30-tasks/weeks/week4-tasks.md) |
| 바꾼 걸 검증하고 싶다 | [테스트 절차](40-tests/MOC-tests.md) |
| 문서를 쓰려고 한다 | [문서 규칙](00-meta/doc-conventions.md) |

## 지도

- **[00-meta](00-meta/doc-conventions.md)** — frontmatter 스키마, 콜아웃, 링크 규칙, 템플릿
- **[10-overview](10-overview/architecture.md)** — 변하지 않는 설명: 구조, 용어, AWS 대응
- **[20-guides](20-guides/install.md)** — 쓰는 법: 설치·API·대시보드·net-helper·트러블슈팅
- **[30-tasks](30-tasks/MOC-tasks.md)** — 할 일과 한 일. 주차별 계획은 [weeks](30-tasks/weeks/week4-tasks.md)
- **[40-tests](40-tests/MOC-tests.md)** — 기능별 검증 절차(자동 테스트 + 수동 절차)
- **[50-bugs](50-bugs/MOC-bugs.md)** — 실제로 겪은 버그의 증상·원인·수정
- **[90-appendix](90-appendix/firecracker-manual/README.md)** — Firecracker 수동 조작 절차(원본 기록)

## 지금 상태

```dataview
TABLE WITHOUT ID
  scope AS "주차",
  length(filter(rows, (r) => r.status = "완료")) AS "완료",
  length(filter(rows, (r) => r.status = "진행 중")) AS "진행 중",
  length(filter(rows, (r) => r.status = "미완료")) AS "미완료"
FROM "30-tasks"
WHERE scope
GROUP BY scope
SORT scope ASC
```

> [!note] Dataview가 안 보이면
> 위 블록이 코드로 보이면 Obsidian 커뮤니티 플러그인 **Dataview**가 없는 것이다.
> GitHub 웹에서는 원래 코드블록으로 보인다 — 그쪽에서는 각 주차 문서의 표를 보면 된다.
