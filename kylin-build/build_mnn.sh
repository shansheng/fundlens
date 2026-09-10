#!/usr/bin/env bash
set -e
export HOME=/home/sheng
SRC=/home/sheng/opt/mnn-src
BUILD=/home/sheng/opt/mnn-build
export CMAKE_POLICY_VERSION_MINIMUM=3.5
rm -rf "$BUILD"; mkdir -p "$BUILD"
cd "$BUILD"
echo "=== cmake configure ==="
cmake "$SRC" \
  -DCMAKE_BUILD_TYPE=Release \
  -DMNN_BUILD_SHARED_LIBS=OFF \
  -DMNN_BUILD_TRAIN=OFF \
  -DMNN_BUILD_DEMO=OFF \
  -DMNN_BUILD_QUANTOOLS=OFF \
  -DMNN_BUILD_CONVERTER=OFF \
  -DMNN_BUILD_BENCHMARK=OFF \
  -DMNN_BUILD_TEST=OFF \
  -DMNN_BUILD_TOOLS=OFF \
  -DMNN_EVALUATION=OFF \
  -DMNN_SEP_BUILD=OFF \
  -DMNN_ARM82=ON \
  -DMNN_OPENCL=OFF -DMNN_VULKAN=OFF -DMNN_CUDA=OFF -DMNN_METAL=OFF -DMNN_COREML=OFF
echo "=== cmake build ($(nproc) jobs) ==="
cmake --build . -j"$(nproc)" --target MNN
echo "=== locate libMNN.a ==="
find "$BUILD" -name "libMNN.a" 2>/dev/null
echo "BUILD_MNN_DONE rc=$?"
