---
tags:
  - firecrab
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 설정 및 secret 관리 구현

## 브랜치 개요

- 브랜치: `feat/configuration-and-secrets` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 환경별 설정을 typed Rust 구조체로 로드하고 시작 전에 상호 제약을 검증함.
- secret은 별도 type과 systemd credential로 다룸.

## Typed config

```rust
#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub http: HttpConfig,
    pub database: DatabaseConfig,
    pub runtime: RuntimeConfig,
    pub network: NetworkConfig,
    pub limits: LimitConfig,
}

pub struct AppSecrets {
    pub token_pepper: SecretBytes,
    pub idempotency_hash_key: SecretBytes,
    pub backup_key: secrecy::SecretString,
    pub snapshot_manifest_key: SecretBytes,
}
```

- non-secret source 우선순위는 package default, config file, allowlist environment, 명시적 CLI 순으로 고정함.
- 어떤 source가 최종값을 바꿨는지는 secret을 제외하고 startup log에 기록함.
- secret 값은 CLI argument에 허용하지 않고 production에서는 broad environment 상속도 차단함.

## 검증

```rust
impl AppConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_loopback_or_tls(&self.http)?;
        validate_subnet_and_gateway(&self.network)?;
        validate_roots_are_absolute_and_owned(&self.runtime)?;
        validate_quota_relationships(&self.limits)?;
        Ok(())
    }
}
```

- path capability와 owner/mode 검증은 host 변경 전에 수행함.
- 서로 겹치는 subnet, world-writable binary, 빈 secret, quota 역전은 시작 실패로 처리함.
- production policy가 encrypted data root를 요구하면 mount identity와 encryption 상태를 startup preflight에서 검증하고 단순 path 이름만으로 통과시키지 않음.

## Secret

- versioned token pepper, idempotency-key HMAC key, TLS private key, backup encryption key와 snapshot manifest HMAC key는 일반 환경 변수보다 systemd credential file을 우선 사용함.
- 목적별 key를 재사용하지 않으며 HMAC key와 pepper를 UTF-8 문자열로 제한하지 않고 길이가 검증된 binary secret type으로 다룸.
- secret type의 `Debug`를 노출하지 않고 error context에도 원문을 넣지 않음.
- key version, fingerprint와 rotation 상태는 secret이 아닌 metadata로 관리함.

- runtime reload는 log level과 일부 quota처럼 안전한 항목만 허용함.
- binary path, data root, signing key 변경은 재시작이 필요함.

## 테스트 및 검증

- 각 invalid config와 secret permission 오류가 process spawn·network 변경 전에 실패해야 함.
- debug dump, panic, metrics와 health 응답에서 secret이 검색되지 않아야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
