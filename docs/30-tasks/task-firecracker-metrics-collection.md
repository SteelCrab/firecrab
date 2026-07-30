---
tags:
  - firecrab
  - firecracker
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# Firecracker metrics 수집 구현

## 브랜치 개요

- 브랜치: `feat/firecracker-metrics-collection` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: VM별 Firecracker metrics FIFO를 지속적으로 drain하고 필요한 지표만 bounded registry에 반영함.

## Reader

```rust
#[derive(Debug, Deserialize)]
struct FirecrackerMetrics {
    utc_timestamp_ms: u64,
    #[serde(flatten)]
    sections: BTreeMap<String, serde_json::Value>,
}

pub async fn collect_metrics<R: AsyncRead + Unpin>(
    vm_id: Uuid,
    reader: R,
    sink: mpsc::Sender<MetricsEnvelope>,
    errors: MetricsReaderCounters,
) -> anyhow::Result<()> {
    let mut decoder = BoundedLineDecoder::new(reader, 256 * 1024);
    while let Some(line) = decoder.next_line().await? {
        let metrics = match serde_json::from_slice(&line) {
            Ok(metrics) => metrics,
            Err(error) => {
                errors.malformed.inc();
                rate_limited_warn(vm_id, &error);
                continue;
            }
        };
        if sink.try_send(MetricsEnvelope { vm_id, metrics }).is_err() {
            errors.dropped.inc();
        }
    }
    Ok(())
}
```

- runtime helper는 jail 내부 경로를 symlink 없이 만들고 실제 FIFO type/owner/mode를 확인한 뒤 Firecracker metrics 설정 전에 `O_NONBLOCK` reader를 준비해 open handshake가 writer 대기로 막히지 않게 함.
- API daemon이 내려가도 계속 drain해 Firecracker writer block을 방지함.
- API에는 versioned bounded stream으로 필요한 frame만 전달하고 연결이 없거나 느리면 helper가 drop count를 누적함.
- `lines()`로 unbounded allocation을 만들지 않고 frame 최대 크기를 읽는 단계에서 강제함.
- oversized frame은 제한된 memory로 끝까지 discard한 뒤 다음 newline부터 다시 동기화함.
- malformed frame 하나 때문에 reader를 종료하지 않으며 writer attach 전 EOF와 VM 종료를 process identity로 구분해 bounded backoff로 reader를 복구함.
- sink가 느릴 때 FIFO drain을 멈추지 않도록 bounded channel의 `try_send`를 사용하고 drop count metric을 증가시킴.

- Firecracker version에 따라 metrics section이 달라질 수 있으므로 수집 단계에서는 나머지 key를 보존하고, 지원 version별 변환기에서 필요한 counter를 명시적으로 추출함.

- Firecracker metrics는 flush 주기에 따라 값이 reset될 수 있으므로 raw 값을 일반 cumulative counter로 오해하지 않음.
- Firecracker version과 flush timestamp를 함께 저장하고 변환기에서 delta/counter 의미를 명시함.
- snapshot에는 metrics/log 설정이 저장되지 않으므로 restore process에서 collector와 path를 먼저 준비하고 logger/metrics를 Firecracker snapshot load 전에 구성함.

## 노출

- `prometheus-client` registry에 CPU exit, API error, block I/O, network packet/error, process 상태를 변환함.
- VM 이름처럼 변경 가능한 고카디널리티 값은 label로 쓰지 않고 UUID와 안정된 template 정도만 사용함.

```rust
pub async fn prometheus(State(state): State<AppState>) -> Result<String, AppError> {
    let mut body = String::new();
    prometheus_client::encoding::text::encode(&mut body, &state.metrics.registry())?;
    Ok(body)
}
```

- metrics endpoint는 관리 network에만 노출하거나 인증을 적용함.

## 테스트 및 검증

- 느린 consumer, 잘못된 JSON, FIFO 재생성, VM 종료를 test함.
- collector 장애가 Firecracker lifecycle을 멈추지 않고 stale series가 retention 이후 제거되어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
