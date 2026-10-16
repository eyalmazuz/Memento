include(FetchContent)

if(NOT MEMENTO_SYSTEM_QCORO)
    set(QCORO_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
    set(QCORO_BUILD_TESTING OFF CACHE BOOL "" FORCE)
    set(QCORO_WITH_QTDBUS OFF CACHE BOOL "" FORCE)
    set(QCORO_WITH_QTNETWORK ON CACHE BOOL "" FORCE)
    set(QCORO_WITH_QTWEBSOCKETS OFF CACHE BOOL "" FORCE)
    set(QCORO_WITH_QTQUICK OFF CACHE BOOL "" FORCE)
    set(QCORO_WITH_QML ON CACHE BOOL "" FORCE)
    set(QCORO_WITH_QTTEST OFF CACHE BOOL "" FORCE)

    set(MEMENTO_QCORO_PATCH_PATH "${CMAKE_CURRENT_SOURCE_DIR}/patch/qcoro-remove-install.patch")
    set(MEMENTO_QCORO_PATCH_COMMAND git clean -fd && git reset --hard && git apply ${MEMENTO_QCORO_PATCH_PATH})
    if(WIN32)
        set(MEMENTO_QCORO_PATCH_COMMAND "")
    endif()
    FetchContent_Declare(
        qcoro
        GIT_REPOSITORY https://github.com/qcoro/qcoro.git
        GIT_TAG 98f23d8d8d67e9e24217d268927faec97c713fb0 # v0.13.0
        PATCH_COMMAND ${MEMENTO_QCORO_PATCH_COMMAND}
    )
    FetchContent_MakeAvailable(qcoro)
endif()

if(MEMENTO_OCR_SUPPORT AND NOT MEMENTO_SYSTEM_MOCR)
    set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
    FetchContent_Declare(
        libmocr
        GIT_REPOSITORY https://github.com/ripose-jp/libmocr.git
        GIT_TAG 24ec081102208d0ff6954c3003df2fb691713bd5
    )
    FetchContent_MakeAvailable(libmocr)
endif()

if(MEMENTO_WHISPER_SUPPORT)
    set(MEMENTO_BUILD_SHARED_LIBS_WAS_SET FALSE)
    if(DEFINED BUILD_SHARED_LIBS)
        set(MEMENTO_BUILD_SHARED_LIBS_WAS_SET TRUE)
        set(MEMENTO_BUILD_SHARED_LIBS_VALUE "${BUILD_SHARED_LIBS}")
    endif()

    set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
    set(WHISPER_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
    set(WHISPER_BUILD_SERVER OFF CACHE BOOL "" FORCE)
    set(WHISPER_BUILD_TESTS OFF CACHE BOOL "" FORCE)
    set(WHISPER_CURL OFF CACHE BOOL "" FORCE)
    set(GGML_CUDA ${MEMENTO_WHISPER_CUDA} CACHE BOOL "" FORCE)
    set(
        GGML_CUDA_FORCE_MMQ
        ${MEMENTO_WHISPER_CUDA_FORCE_MMQ}
        CACHE BOOL "" FORCE
    )
    set(GGML_VULKAN ${MEMENTO_WHISPER_VULKAN} CACHE BOOL "" FORCE)
    set(GGML_BLAS ${MEMENTO_WHISPER_BLAS} CACHE BOOL "" FORCE)
    set(
        GGML_BLAS_VENDOR
        "${MEMENTO_WHISPER_BLAS_VENDOR}"
        CACHE STRING "" FORCE
    )
    if(MEMENTO_WHISPER_CUDA)
        if(MEMENTO_WHISPER_CUDA_ARCHITECTURES)
            set(
                CMAKE_CUDA_ARCHITECTURES
                "${MEMENTO_WHISPER_CUDA_ARCHITECTURES}"
                CACHE STRING "" FORCE
            )
        endif()
    endif()
    FetchContent_Declare(
        whispercpp
        GIT_REPOSITORY https://github.com/ggml-org/whisper.cpp.git
        GIT_TAG v1.9.4
    )
    FetchContent_MakeAvailable(whispercpp)

    if(MEMENTO_BUILD_SHARED_LIBS_WAS_SET)
        set(
            BUILD_SHARED_LIBS "${MEMENTO_BUILD_SHARED_LIBS_VALUE}"
            CACHE BOOL "" FORCE
        )
    else()
        unset(BUILD_SHARED_LIBS CACHE)
        unset(BUILD_SHARED_LIBS)
    endif()
endif()

if(MEMENTO_ASR_BACKEND_ONNX)
    find_path(
        ONNXRUNTIME_INCLUDE_DIR
        NAMES onnxruntime_cxx_api.h
        HINTS "${ONNXRUNTIME_ROOT}"
        PATH_SUFFIXES include include/onnxruntime include/onnxruntime/core/session
    )
    find_library(
        ONNXRUNTIME_LIBRARY
        NAMES onnxruntime
        HINTS "${ONNXRUNTIME_ROOT}"
        PATH_SUFFIXES lib lib64
    )

    if(NOT ONNXRUNTIME_INCLUDE_DIR OR NOT ONNXRUNTIME_LIBRARY)
        set(ONNXRUNTIME_VERSION "1.22.0")
        if(CMAKE_SYSTEM_NAME STREQUAL "Linux" AND CMAKE_SYSTEM_PROCESSOR MATCHES "x86_64|amd64")
            if(MEMENTO_ONNX_CUDA)
                set(ONNXRUNTIME_ARCHIVE "onnxruntime-linux-x64-gpu-${ONNXRUNTIME_VERSION}.tgz")
            else()
                set(ONNXRUNTIME_ARCHIVE "onnxruntime-linux-x64-${ONNXRUNTIME_VERSION}.tgz")
            endif()
        elseif(CMAKE_SYSTEM_NAME STREQUAL "Linux" AND CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
            set(ONNXRUNTIME_ARCHIVE "onnxruntime-linux-aarch64-${ONNXRUNTIME_VERSION}.tgz")
        elseif(APPLE AND CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
            set(ONNXRUNTIME_ARCHIVE "onnxruntime-osx-arm64-${ONNXRUNTIME_VERSION}.tgz")
        elseif(APPLE)
            set(ONNXRUNTIME_ARCHIVE "onnxruntime-osx-x86_64-${ONNXRUNTIME_VERSION}.tgz")
        elseif(WIN32)
            if(MEMENTO_ONNX_CUDA)
                set(ONNXRUNTIME_ARCHIVE "onnxruntime-win-x64-gpu-${ONNXRUNTIME_VERSION}.zip")
            else()
                set(ONNXRUNTIME_ARCHIVE "onnxruntime-win-x64-${ONNXRUNTIME_VERSION}.zip")
            endif()
        else()
            message(FATAL_ERROR "Unsupported platform for automatic ONNX Runtime download; please set -DONNXRUNTIME_ROOT=/path/to/onnxruntime")
        endif()

        FetchContent_Declare(
            onnxruntime_prebuilt
            URL "https://github.com/microsoft/onnxruntime/releases/download/v${ONNXRUNTIME_VERSION}/${ONNXRUNTIME_ARCHIVE}"
            DOWNLOAD_EXTRACT_TIMESTAMP TRUE
        )
        FetchContent_MakeAvailable(onnxruntime_prebuilt)

        set(ONNXRUNTIME_INCLUDE_DIR "${onnxruntime_prebuilt_SOURCE_DIR}/include")
        if(WIN32)
            set(ONNXRUNTIME_LIBRARY "${onnxruntime_prebuilt_SOURCE_DIR}/lib/onnxruntime.lib")
        elseif(APPLE)
            set(ONNXRUNTIME_LIBRARY "${onnxruntime_prebuilt_SOURCE_DIR}/lib/libonnxruntime.dylib")
        else()
            set(ONNXRUNTIME_LIBRARY "${onnxruntime_prebuilt_SOURCE_DIR}/lib/libonnxruntime.so")
        endif()
    endif()

    get_filename_component(ONNXRUNTIME_LIB_DIR "${ONNXRUNTIME_LIBRARY}" DIRECTORY)
    message(STATUS "Found ONNX Runtime: include=${ONNXRUNTIME_INCLUDE_DIR}, lib=${ONNXRUNTIME_LIBRARY}")

    if(NOT TARGET onnxruntime::onnxruntime)
        add_library(onnxruntime::onnxruntime SHARED IMPORTED GLOBAL)
        set_target_properties(
            onnxruntime::onnxruntime
            PROPERTIES
                INTERFACE_INCLUDE_DIRECTORIES "${ONNXRUNTIME_INCLUDE_DIR}"
                IMPORTED_LOCATION "${ONNXRUNTIME_LIBRARY}"
                IMPORTED_NO_SONAME FALSE
        )
        if(UNIX AND NOT APPLE)
            target_link_options(
                onnxruntime::onnxruntime
                INTERFACE "-Wl,-rpath,${ONNXRUNTIME_LIB_DIR}"
            )
        elseif(APPLE)
            target_link_options(
                onnxruntime::onnxruntime
                INTERFACE "-Wl,-rpath,${ONNXRUNTIME_LIB_DIR}"
            )
        endif()
    endif()
endif()
