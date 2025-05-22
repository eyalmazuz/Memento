if(ASR_SUPPORT)
    add_library(
        asrmodel STATIC
        asrmodel.cpp
        asrmodel.h
    )
    target_compile_features(asrmodel PUBLIC cxx_std_20)
    target_compile_options(asrmodel PRIVATE ${MEMENTO_COMPILER_FLAGS})
    target_include_directories(asrmodel PRIVATE ${MEMENTO_INCLUDE_DIRS})
    target_link_libraries(
        asrmodel
        PRIVATE whisper
        PRIVATE Qt6::Concurrent
        PRIVATE Qt6::Gui
        PUBLIC Qt6::Core
    )
endif()
