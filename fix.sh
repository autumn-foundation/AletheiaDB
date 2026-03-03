#!/bin/bash
sed -i 's/if vt_start <= time.wallclock() {/if vt_start <= time.wallclock() \&\& vt_start >= best_time {/' src/experimental/omen.rs
sed -i '/if vt_start >= best_time {/d' src/experimental/omen.rs
sed -i 's/if let Some(val) = v.properties.get(property) {/if let Some(val) = v.properties.get(property) \&\& let Some(vec) = val.as_vector() {/' src/experimental/omen.rs
sed -i '/if let Some(vec) = val.as_vector() {/d' src/experimental/omen.rs

# Adjusting closing brackets manually is error prone with sed, let's use python or just do it inline
