package main

import (
	"log"
	"net/http"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/vms", listVMs)

	addr := "0.0.0.0:3001"
	log.Printf("[INFO] Listening on http://%s", addr)
	log.Fatal(http.ListenAndServe(addr, mux))
}
