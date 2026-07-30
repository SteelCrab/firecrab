---
tags:
  - firecrab
  - vm
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# MicroVM 목록 조회 API 구현

## 브랜치 개요

- 브랜치: `feat/microvm-list-api`
- 커밋: `b215e74 feat: add paginated VM list API`
- 상태: 구현 브랜치 존재
- 변경 규모: 9개 파일, 202줄 추가, 6줄 삭제
- 목적: `GET /api/vms`에서 삭제되지 않은 VM을 이름과 UUID 순으로 안정적으로 정렬해 cursor page로 반환한다.

## 저장소 API

```rust
impl Store {
    pub async fn list_vms(
        &self,
        cursor: Option<VmCursor>,
        limit: PageLimit,
    ) -> Result<VmPage, StoreError> {
        sqlx::query_as::<_, VmListRow>(
            r#"SELECT id, name, state, template_name, template_version,
                      cpu, ram, created_at, updated_at
               FROM vms
               WHERE state <> 'deleted'
                 AND (?1 IS NULL OR name > ?1 OR (name = ?1 AND id > ?2))
               ORDER BY name ASC, id ASC
               LIMIT ?3"#,
        )
        .bind(cursor.as_ref().map(|value| value.name.as_str()))
        .bind(cursor.as_ref().map(|value| value.id.to_string()))
        .bind(i64::from(limit.get()) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)
        .and_then(|rows| VmPage::from_rows(rows, limit))
    }
}
```

## Handler와 route

```rust
pub async fn list_vms(
    State(state): State<AppState>,
    Query(query): Query<ListVmQuery>,
) -> Result<Json<VmPage>, AppError> {
    let cursor = query.cursor.map(VmCursor::decode).transpose()?;
    let limit = PageLimit::new(query.limit.unwrap_or(100))?;
    Ok(Json(state.store.list_vms(cursor, limit).await?))
}

let app = Router::new().route(
    "/api/vms",
    get(list_vms).post(create_vm),
);
```

- `PageLimit`은 `1..=200`만 허용하고 범위 밖 값은 조용히 clamp하지 않고 `400 invalid_query`로 반환함.
- cursor는 `(name, id)`를 담은 opaque base64url 값이며 길이와 decode 결과를 제한함.
- client가 값을 변조해도 임의 SQL이 되지 않도록 항상 bind parameter를 사용함.

- 빈 결과는 `200 OK`와 `{"items":[],"nextCursor":null}`로 반환함.
- 내부 DB 오류 메시지는 로그에 남기고 클라이언트에는 일반화된 `internal_error`만 노출함.

- `VmListRow`는 DB 전용 type이고 `VmPage` item은 shared API DTO로 명시적으로 변환함.
- PID, disk/runtime generation, raw `last_error`와 host path가 SELECT나 직렬화에 우연히 추가되지 않게 response snapshot test를 둠.

## 테스트 및 검증

- 빈 DB에서 `items`가 `[]`이고 `nextCursor`가 `null`인지 확인한다.
- 같은 이름의 레코드는 UUID 오름차순으로 정렬되는지 확인한다.
- `deleted` 상태가 기본 목록에서 제외되는지 확인한다.
- page 경계에서 누락·중복이 없고 limit `0`, `201`, malformed cursor가 안전하게 처리되는지 확인한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
