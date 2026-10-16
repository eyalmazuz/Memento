if(NOT BUILD_TESTING OR NOT MEMENTO_ASR_SUPPORT)
    return()
endif()

add_executable(
    mpv_memory_subtitles_fake_asr
    mpv-memory-subtitles/fake_asr_test.cpp
)
target_compile_features(mpv_memory_subtitles_fake_asr PRIVATE cxx_std_20)
target_include_directories(
    mpv_memory_subtitles_fake_asr
    PRIVATE ${MEMENTO_INCLUDE_DIRS}
)
target_compile_options(
    mpv_memory_subtitles_fake_asr
    PRIVATE ${MEMENTO_COMPILER_FLAGS}
)
target_link_libraries(
    mpv_memory_subtitles_fake_asr
    PRIVATE mpvplayer
    PRIVATE Qt6::Concurrent
)

add_executable(
    asr_controller_retry_test
    mpv-memory-subtitles/asr_controller_retry_test.cpp
)
target_compile_features(asr_controller_retry_test PRIVATE cxx_std_20)
target_include_directories(
    asr_controller_retry_test
    PRIVATE ${MEMENTO_INCLUDE_DIRS}
)
target_compile_options(
    asr_controller_retry_test
    PRIVATE ${MEMENTO_COMPILER_FLAGS}
)
target_link_libraries(
    asr_controller_retry_test
    PRIVATE asrtranscriber
    PRIVATE context
    PRIVATE managers
    PRIVATE mpvplayer
    PRIVATE Qt6::Concurrent
)

add_executable(
    onnx_asr_unit_test
    mpv-memory-subtitles/onnx_asr_unit_test.cpp
)
target_compile_features(onnx_asr_unit_test PRIVATE cxx_std_20)
target_include_directories(
    onnx_asr_unit_test
    PRIVATE ${MEMENTO_INCLUDE_DIRS}
)
target_compile_options(
    onnx_asr_unit_test
    PRIVATE ${MEMENTO_COMPILER_FLAGS}
)
target_link_libraries(
    onnx_asr_unit_test
    PRIVATE asrtranscriber
    PRIVATE Qt6::Core
)

get_filename_component(MPV_SDK "${mpv_INCLUDE_DIR}" DIRECTORY)
get_filename_component(MPV_LIBRARY_DIR "${mpv_LIBRARY}" DIRECTORY)
set(
    MPV_TEST_ENV
    "MPV_SDK=${MPV_SDK}"
    "MPV_INCLUDE_DIR=${mpv_INCLUDE_DIR}"
    "MPV_LIBRARY_DIR=${MPV_LIBRARY_DIR}"
    "QT_QPA_PLATFORM=offscreen"
)
add_test(
    NAME mpv-memory-subtitles
    COMMAND
        sh
        "${CMAKE_CURRENT_SOURCE_DIR}/mpv-memory-subtitles/run.sh"
        "$<TARGET_FILE:mpv_memory_subtitles_fake_asr>"
        "$<TARGET_FILE:asr_controller_retry_test>"
)
set_tests_properties(
    mpv-memory-subtitles
    PROPERTIES
        ENVIRONMENT "${MPV_TEST_ENV}"
        TIMEOUT 30
)
add_test(
    NAME onnx-asr-unit-test
    COMMAND "$<TARGET_FILE:onnx_asr_unit_test>"
)
set_tests_properties(
    onnx-asr-unit-test
    PROPERTIES
        TIMEOUT 15
)
