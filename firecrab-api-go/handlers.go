package main

import (
	"cmp"
	"encoding/json"
	"net/http"
	"slices"
	"strings"
)

func listVMs(w http.ResponseWriter, r *http.Request) {
	vms := loadVMs()

	list := make([]VMRecord, 0, len(vms))
	for _, vm := range vms {
		list = append(list, vm)
	}
	slices.SortFunc(list, func(a, b VMRecord) int {
		return cmp.Or(strings.Compare(a.Name, b.Name), strings.Compare(a.ID, b.ID))
	})

	w.Header().Set("Access-Control-Allow-Origin", "*")
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(list)
}
