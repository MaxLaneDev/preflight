package main

import "fmt"

func sum(values []int) int {
	result := 0
	for _, value := range values {
		result += value
	}
	return result
}
func main() { fmt.Println(sum([]int{10, 20})) }
