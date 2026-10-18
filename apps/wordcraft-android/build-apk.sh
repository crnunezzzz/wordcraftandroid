#!/usr/bin/env bash
set -euo pipefail
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$app_dir/../.." && pwd)"
: "${ANDROID_HOME:?Set ANDROID_HOME to your Android SDK directory}"
: "${ANDROID_NDK_HOME:=$ANDROID_HOME/ndk/28.2.13676358}"
export ANDROID_NDK_HOME
ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
if [[ "$(uname -s)" == Darwin ]]; then ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin"; fi
export CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android26-clang"
export CXX_aarch64_linux_android="$ndk_bin/aarch64-linux-android26-clang++"
export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
export CXXFLAGS_aarch64_linux_android="-DANDROID_SUPPORT_FLEXIBLE_PAGE_SIZES=1"
cd "$repo_dir"
native_profile="${CRAFT_ANDROID_PROFILE:-android}"
cargo rustc --profile "$native_profile" --locked -p wordcraft-android --target aarch64-linux-android -- -C link-arg=-Wl,-z,max-page-size=16384
native_dir="$app_dir/android/app/src/main/jniLibs/arm64-v8a"
mkdir -p "$native_dir"
cp "${CARGO_TARGET_DIR:-$repo_dir/target}/aarch64-linux-android/$native_profile/libwordcraft_android.so" "$native_dir/"
cp "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$(basename -- "$(dirname -- "$ndk_bin")")/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" "$native_dir/"
"$ndk_bin/llvm-strip" --strip-unneeded "$native_dir/libwordcraft_android.so" "$native_dir/libc++_shared.so"
cd "$app_dir/android"
./gradlew --no-daemon assembleDebug
printf '%s\n' "$app_dir/android/app/build/outputs/apk/debug/app-debug.apk"
