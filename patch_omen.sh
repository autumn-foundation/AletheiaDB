#!/bin/bash
sed -i 's/if let Some(val) = v.properties.get(property) {/if let Some(val) = v.properties.get(property) {/g' src/experimental/omen.rs
