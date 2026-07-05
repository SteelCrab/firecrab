package main

type VMRecord struct {
	ID       string  `json:"id"`
	Name     string  `json:"name"`
	State    string  `json:"state"`
	Template string  `json:"template"`
	CPU      float64 `json:"cpu"`
	RAM      uint32  `json:"ram"`
}
