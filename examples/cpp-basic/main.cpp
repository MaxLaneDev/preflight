#include <cassert>
#include <numeric>
#include <vector>
int main() {
    const std::vector<int> values{10, 20, -5};
    assert(std::accumulate(values.begin(), values.end(), 0) == 25);
}
