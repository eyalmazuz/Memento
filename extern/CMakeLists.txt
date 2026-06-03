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
        GIT_TAG ef1231ae490465b4f592ca326eee6d56a3d802ae # v0.12.0
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
        GIT_TAG v1.8.6
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
