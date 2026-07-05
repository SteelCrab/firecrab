package main

import (
	"encoding/json"
	"os"
)

// firecrab-api(Rust)가 쓰는 데이터 파일을 공유한다. firecrab-api-go/에서 실행해야 한다.
const dataFile = "../firecrab-api/data/vms.json"

func loadVMs() map[string]VMRecord {
	content, err := os.ReadFile(dataFile)
	if err != nil {
		return map[string]VMRecord{}
	}
	var vms map[string]VMRecord
	if err := json.Unmarshal(content, &vms); err != nil {
		return map[string]VMRecord{}
	}
	return vms
}
