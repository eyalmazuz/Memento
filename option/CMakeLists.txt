# Apple Build Options
option(MEMENTO_BUNDLE "Toggle when building macOS Bundles" OFF)
set(MEMENTO_CODESIGN_IDENTITY "" CACHE STRING "macOS code signing identity for the Bundle")
set(MEMENTO_CODESIGN_KEYCHAIN "" CACHE FILEPATH "macOS keychain containing the code signing identity")

# Release Options
option(MEMENTO_RELEASE_BUILD "Toggle to build with release settings" OFF)

# Debugging
option(MEMENTO_WERROR "Use -Werror when compiling" OFF)
option(MEMENTO_ASAN "Enable the address sanitizer" OFF)
option(MEMENTO_WARN_UNSUPPORTED_CSS "Enable log messages for unsupported CSS" OFF)

# Use Local System Libraries
option(MEMENTO_SYSTEM_MOCR "Use the local installation of libmocr instead of FetchContent" OFF)
option(MEMENTO_SYSTEM_QCORO "Use the local installation of QCoro instead of FetchContent" OFF)

# Features
option(MEMENTO_OCR_SUPPORT "Support for OCR through MangaOCR" OFF)
option(MEMENTO_MECAB_SUPPORT "Support for deconjugation with MeCab" OFF)

set(
    ASR_BACKENDS
    "whisper"
    CACHE STRING
    "Semicolon- or comma-separated list of ASR runtime backends to build (e.g., whisper;onnx)"
)
option(MEMENTO_WHISPER_SUPPORT "Support for transcription through whisper.cpp" ON)
option(MEMENTO_ONNX_SUPPORT "Support for transcription through ONNX Runtime" OFF)
option(MEMENTO_ONNX_CUDA "Use the GPU/CUDA build of ONNX Runtime when fetching prebuilt binaries" OFF)
set(ONNXRUNTIME_ROOT "" CACHE PATH "Optional path to a local ONNX Runtime installation")

set(_asr_backends_raw "${ASR_BACKENDS}")
string(REPLACE "," ";" _asr_backends_list "${_asr_backends_raw}")
set(_asr_whisper OFF)
set(_asr_onnx OFF)

foreach(_backend IN LISTS _asr_backends_list)
    string(STRIP "${_backend}" _backend_clean)
    string(TOLOWER "${_backend_clean}" _backend_lower)
    if(_backend_lower STREQUAL "" OR _backend_lower STREQUAL "none" OR _backend_lower STREQUAL "off")
        continue()
    elseif(_backend_lower STREQUAL "whisper")
        set(_asr_whisper ON)
    elseif(_backend_lower STREQUAL "onnx" OR _backend_lower STREQUAL "onnxruntime")
        set(_asr_onnx ON)
    else()
        message(FATAL_ERROR "Unknown ASR backend '${_backend_clean}' in ASR_BACKENDS='${ASR_BACKENDS}'. Supported backends: whisper, onnx")
    endif()
endforeach()

if(ASR_BACKENDS STREQUAL "whisper" AND NOT MEMENTO_WHISPER_SUPPORT)
    set(_asr_whisper OFF)
endif()

set(MEMENTO_ASR_BACKEND_WHISPER ${_asr_whisper} CACHE INTERNAL "Build Whisper ASR backend")
set(MEMENTO_ASR_BACKEND_ONNX ${_asr_onnx} CACHE INTERNAL "Build ONNX ASR backend")
set(MEMENTO_WHISPER_SUPPORT ${MEMENTO_ASR_BACKEND_WHISPER} CACHE BOOL "Support for transcription through whisper.cpp" FORCE)
set(MEMENTO_ONNX_SUPPORT ${MEMENTO_ASR_BACKEND_ONNX} CACHE BOOL "Support for transcription through ONNX Runtime" FORCE)

if(MEMENTO_ASR_BACKEND_WHISPER OR MEMENTO_ASR_BACKEND_ONNX)
    set(MEMENTO_ASR_SUPPORT ON CACHE INTERNAL "Any ASR backend enabled")
else()
    set(MEMENTO_ASR_SUPPORT OFF CACHE INTERNAL "Any ASR backend enabled")
endif()

message(STATUS "ASR backends enabled: whisper=${MEMENTO_ASR_BACKEND_WHISPER}, onnx=${MEMENTO_ASR_BACKEND_ONNX}")

option(MEMENTO_WHISPER_CUDA "Build whisper.cpp with the ggml CUDA backend" OFF)
option(MEMENTO_WHISPER_VULKAN "Build whisper.cpp with the ggml Vulkan backend" OFF)
option(MEMENTO_WHISPER_BLAS "Build whisper.cpp with the ggml BLAS backend" OFF)
option(MEMENTO_WHISPER_CUDA_FORCE_MMQ "Force ggml CUDA MMQ kernels instead of cuBLAS" OFF)
set(MEMENTO_WHISPER_BLAS_VENDOR "Generic" CACHE STRING "BLAS vendor used by ggml when MEMENTO_WHISPER_BLAS is ON")
set(MEMENTO_WHISPER_CUDA_ARCHITECTURES "" CACHE STRING "CUDA architectures for whisper.cpp, for example 75 for GTX 1650; leave empty for CMake default")

# Modifications
option(MEMENTO_QAPPLICATION "Builds Memento with a QApplication instead of QGuiApplication" OFF)
option(MEMENTO_WIN32_EXECUTABLE "Disable console output on Windows" ON)
