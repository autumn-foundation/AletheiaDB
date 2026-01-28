# Bolt's Journal

## 2024-05-23 - [SIMD Loop Unrolling]
**Learning:** Modern CPUs (AVX2/FMA) benefit significantly from loop unrolling (processing 32+ floats/iter) to break dependency chains in accumulation operations. The standard 8-float loop is limited by FMA latency (4-5 cycles), not throughput.
**Action:** When implementing SIMD accumulations (dot product, sum of squares), always unroll loops to use multiple accumulators (e.g., 4 independent sums) to saturate the FMA pipeline.
