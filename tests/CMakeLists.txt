if(NOT BUILD_TESTING OR NOT MEMENTO_WHISPER_SUPPORT)
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
)
set_tests_properties(
    mpv-memory-subtitles
    PROPERTIES
        ENVIRONMENT "${MPV_TEST_ENV}"
)
