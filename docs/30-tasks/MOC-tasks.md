---
tags:
  - firecrab
  - moc
  - plan
updated: 2026-07-30
---

# 태스크

> [!summary] 이 폴더의 규칙
> 한 파일 = 한 태스크. 상태는 각 문서의 `status` frontmatter가 단일 출처이고,
> 아래 표는 그걸 모아 보여줄 뿐이다.
> **주차 문서**([weeks](weeks/week4-tasks.md))는 순서와 이유를 담고, 여기서는 상태만 본다.

`status` 값은 넷뿐이다 — `미완료` · `진행 중` · `완료` · `보류`.
`보류`는 어느 주차에도 안 걸린 것(범위 밖으로 미룬 것)을 뜻한다.

## 주차 문서

| 주차 | 내용 |
|---|---|
| [1주차 결과](weeks/week1-result.md) | 초기 구축 결과 |
| [2주차](weeks/week2-tasks.md) | API·상태 모델·프로세스 수명주기 |
| [3주차](weeks/week3-tasks.md) | 네트워크·SSH·UI |
| [4주차](weeks/week4-tasks.md) | Host·MicroStorage·MicroNetwork·M2Image·격리·관측 |
| [5주차](weeks/week5-tasks.md) | 보안·배포·복구 |
| [7주차](weeks/week7-tasks.md) | Snapshot·이월 항목 |

## 지금 할 일

```dataview
TABLE status AS "상태", scope AS "주차", updated AS "갱신"
FROM "30-tasks"
WHERE status != "완료" AND status != "보류" AND file.name != this.file.name
SORT scope ASC, file.name ASC
```

## 주차별

```dataview
TABLE WITHOUT ID
  scope AS "주차",
  length(rows) AS "합계",
  length(filter(rows, (r) => r.status = "완료")) AS "완료"
FROM "30-tasks"
WHERE scope
GROUP BY scope
SORT scope ASC
```

## 영역별

```dataview
TABLE WITHOUT ID
  tag AS "영역",
  length(rows) AS "문서 수"
FROM "30-tasks"
FLATTEN file.tags AS tag
WHERE tag != "#firecrab"
GROUP BY tag
SORT length(rows) DESC
```

## 보류 (어느 주차에도 안 걸림)

범위 밖으로 미룬 것들. 재개할 때를 대비해 문서만 남겨둔다.

- [guest agent · vsock provisioning](task-guest-agent-vsock-provisioning.md)
- [VM별 SSH identity](task-vm-ssh-identity.md)
- [VM 접속 정보 조회 API](task-vm-connection-api.md)
- [Network·SSH·UI 통합 테스트](task-network-ssh-ui-tests.md)
- [MicroVM start API](task-microvm-start-api.md) · [stop API](task-microvm-stop-api.md)
- [VM 리소스 설정](task-vm-resource-configuration.md)

```dataview
LIST
FROM "30-tasks"
WHERE status = "보류"
SORT file.name ASC
```

## 완료

```dataview
TABLE scope AS "주차", updated AS "갱신"
FROM "30-tasks"
WHERE status = "완료"
SORT updated DESC
```
