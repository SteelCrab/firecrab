# 5주차 권장 Tasks - Production Security, Delivery, and Recovery

Week 4의 격리·관측이 검증된 뒤 진행함(snapshot은 7주차로 이월). 재현 가능한 image 공급망, API 접근 제어, quota, backup과 service 배포·upgrade를 묶어 운영 가능한 release 기준을 완성함. 코드 조각은 설계 골격이며 pinned toolchain과 dependency로 compile/test해 적용함.

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| 미완료 | [설정 및 secret 관리 구현](task-configuration-and-secrets.md) | file, environment, systemd credential 설정을 typed config로 로드하고 startup에 검증함. | 잘못된 설정은 변경 전에 실패하고 secret이 Debug, error, metrics에 노출되지 않음. | `firecrab-api/src/config.rs`, `docs/configuration.md` |
| 미완료 | [재현 가능한 template build pipeline 구현](task-reproducible-template-builds.md) | 고정된 입력과 package 목록으로 kernel·rootfs image를 반복 생성함. | 같은 build manifest가 같은 filesystem 내용을 만들고 KVM·boot·DNS·SSH 검증을 통과함. | `firecrab-image-builder/src`, `templates/*.toml` |
| 미완료 | [Template 무결성 검증 및 승격 구현](task-template-integrity-and-promotion.md) | checksum과 signature를 검증한 versioned template만 active registry로 승격함. | 변조 artifact는 거부되고 staging, active, rollback 전환이 원자적으로 수행됨. | `firecrab-api/src/templates.rs`, `firecrab-admin/src/templates.rs` |
| 미완료 | [관리 API 인증 구현](task-api-authentication.md) | 고엔트로피 opaque token의 발급, HMAC 저장, 만료, 폐기와 rotation을 구현함. | 미인증 요청은 lifecycle handler 전에 거부되고 token 원문이 DB와 log에 남지 않음. | `firecrab-api/src/authentication.rs`, `firecrab-admin/src/tokens.rs` |
| 미완료 | [API 권한 및 감사 기록 구현](task-api-authorization-and-audit.md) | viewer, operator, admin 역할별 action을 제한하고 모든 관리 결정을 기록함. | 허용·거부 작업이 actor, resource, request ID, outcome과 함께 조회 가능하며 audit 수정이 제한됨. | `firecrab-api/src/authorization.rs`, `firecrab-api/src/audit.rs` |
| 미완료 | [웹 인증 및 역할 기반 UI 구현](task-authentication-authorization-ui.md) | Rust/Wasm UI에 same-origin server session과 역할별 route·action 제어를 적용함. | token을 browser storage에 남기지 않고 CSRF/CSP, `401`, `403`, 만료와 rotation을 처리하며 서버 권한이 최종 적용됨. | `firecrab-frontend/src/auth.rs`, `firecrab-frontend/src/components/session.rs`, `firecrab-api/src/session.rs` |
| 미완료 | [리소스 quota 및 admission control 구현](task-resource-quotas-and-admission.md) | VM 수, vCPU, RAM, logical/physical disk·snapshot과 동시 operation 한도를 operation transaction에서 예약함. | 동시 요청과 reflink COW에서도 quota 초과가 없고 실제 정리 뒤 reservation이 정확히 반환됨. | `firecrab-api/src/admission.rs`, `firecrab-api/src/persistence.rs` |
| 미완료 | [Graceful shutdown 및 maintenance mode 구현](task-graceful-shutdown-and-maintenance.md) | 새 변경 요청을 즉시 차단하고 진행 중 operation을 drain·복구 가능한 상태로 종료함. | SIGTERM 중 DB 장애에도 intake가 닫히고 재시작 후 일시 shutdown lease는 정리되며 operator maintenance만 유지됨. | `firecrab-api/src/shutdown.rs`, `firecrab-api/src/maintenance.rs` |
| 미완료 | [Backup 및 restore 구현](task-backup-and-restore.md) | SQLite, VM disk generation, immutable template/snapshot, 설정과 external dependency inventory를 암호화 bundle로 생성·복원함. | DB 참조 누락, archive 탈출과 key mismatch가 차단되고 검증된 self-contained backup만 offline staging을 거쳐 복원됨. | `firecrab-admin/src/backup.rs`, `docs/operations/backup-restore.md` |
| 미완료 | [웹 운영 관리 콘솔 구현](task-operations-admin-console.md) | health, quota, audit, maintenance와 backup operation을 관리자 UI에서 조회·실행함. | secret과 host path를 노출하지 않고 위험 작업의 권한·확인·진행 상태·감사 기록이 일관되게 처리됨. | `firecrab-frontend/src/components/admin`, `firecrab-frontend/src/api/admin.rs` |
| 미완료 | [패키징·systemd·upgrade 구현](task-packaging-systemd-upgrades.md) | API와 helper binary, service 계정, directory, unit, migration을 versioned package로 배포함. | 신규 설치, 재설치, upgrade, rollback에서 권한과 데이터가 보존되고 readiness가 정상화됨. | `packaging/`, `firecrab-installer/`, `docs/operations/upgrade.md` |
| 미완료 | [Release 보안 및 운영 검증 구현](task-release-security-and-operations.md) | CI 품질 gate, dependency 정책, SBOM, 서명, load·soak·복구 test와 runbook을 구성함. | 서명된 artifact와 provenance가 생성되고 장시간·장애 test 후 SLO와 cleanup 기준을 충족함. | `.github/workflows/`, `docs/operations/`, release artifacts |

## 4주차에서 이월 (2026-07-29)

- [ ] [cursor pagination](task-microvm-list-api.md) — VM 수가 늘었을 때 목록 API가 견디게
- [ ] [soft delete](task-microvm-delete-api.md) — `deleting`/`deleted` 상태. 삭제 이력이 남아야 감사 가능
