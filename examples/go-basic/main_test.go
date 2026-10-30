package main

import "testing"

func TestSum(t *testing.T) {
	for _, tc := range []struct {
		values []int
		want   int
	}{
		{nil, 0}, {[]int{10, 20}, 30}, {[]int{-5, 2}, -3},
	} {
		if got := sum(tc.values); got != tc.want {
			t.Fatalf("sum(%v) = %d, want %d", tc.values, got, tc.want)
		}
	}
}
