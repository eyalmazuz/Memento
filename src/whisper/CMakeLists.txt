add_library(
    whispertranscriber
    whispercontroller.cpp
    whispercontroller.h
)
if(MEMENTO_WHISPER_SUPPORT)
    target_sources(
        whispertranscriber
        PRIVATE whispermodel.cpp
        PRIVATE whispermodel.h
    )
endif()
target_compile_features(whispertranscriber PUBLIC cxx_std_20)
target_compile_options(whispertranscriber PRIVATE ${MEMENTO_COMPILER_FLAGS})
target_include_directories(whispertranscriber PRIVATE ${MEMENTO_INCLUDE_DIRS})
target_link_libraries(
    whispertranscriber
    PRIVATE "$<$<BOOL:${MEMENTO_WHISPER_SUPPORT}>:whisper>"
    PRIVATE context
    PRIVATE mpvplayer
    PRIVATE Qt6::Concurrent
    PRIVATE settings
    PRIVATE subtitle
    PUBLIC "$<$<BOOL:${MEMENTO_SYSTEM_QCORO}>:QCoro::Coro>"
    PUBLIC "$<$<BOOL:${MEMENTO_SYSTEM_QCORO}>:QCoro::Network>"
    PUBLIC "$<$<BOOL:${MEMENTO_SYSTEM_QCORO}>:QCoro::Qml>"
    PUBLIC "$<$<NOT:$<BOOL:${MEMENTO_SYSTEM_QCORO}>>:QCoro6Coro>"
    PUBLIC "$<$<NOT:$<BOOL:${MEMENTO_SYSTEM_QCORO}>>:QCoro6Network>"
    PUBLIC "$<$<NOT:$<BOOL:${MEMENTO_SYSTEM_QCORO}>>:QCoro6Qml>"
    PUBLIC Qt6::Core
    PUBLIC Qt6::Network
)
