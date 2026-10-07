#include <algorithm>
#include <vector>

// Sorts the three values with the C++ standard library and packs them into
// one number, so the test sees both C++ code and libstdc++/libc++ linking.
extern "C" int sum_sorted(int a, int b, int c) {
  std::vector<int> v{a, b, c};
  std::sort(v.begin(), v.end());
  return v[0] * 100 + v[1] * 10 + v[2];
}
