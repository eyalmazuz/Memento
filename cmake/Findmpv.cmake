include(FindPackageHandleStandardArgs)
include(FetchContent)

find_package(PkgConfig QUIET)
if(PKG_CONFIG_FOUND)
    pkg_check_modules(PC_MPV QUIET mpv)
endif()

set(_mpv_custom_hints
    ${MPV_SDK}
    $ENV{MPV_SDK}
    ${MPV_ROOT}
    $ENV{MPV_ROOT}
)

if(NOT MEMENTO_SYSTEM_MPV)
    if(_mpv_custom_hints)
        find_library(
            mpv_LIBRARY
            NAMES mpv
            HINTS ${_mpv_custom_hints}
            PATH_SUFFIXES lib lib64
            NO_DEFAULT_PATH
        )
        find_path(
            mpv_INCLUDE_DIR
            NAMES mpv/client.h mpv/render_gl.h
            HINTS ${_mpv_custom_hints}
            PATH_SUFFIXES include
            NO_DEFAULT_PATH
        )
    endif()

    if(NOT mpv_LIBRARY OR NOT mpv_INCLUDE_DIR)
        if(CMAKE_SYSTEM_NAME STREQUAL "Linux" AND CMAKE_SYSTEM_PROCESSOR MATCHES "x86_64|amd64")
            message(STATUS "Fetching prebuilt libmpv SDK with in-memory subtitle track support...")
            FetchContent_Declare(
                mpv_patched
                URL "https://github.com/eyalmazuz/mpv/releases/download/v2.5.0-subtitles-15d25e0f/mpv-in-memory-subtitles-x86_64-linux.tar.gz"
                DOWNLOAD_EXTRACT_TIMESTAMP TRUE
            )
            FetchContent_MakeAvailable(mpv_patched)

            set(mpv_INCLUDE_DIR "${mpv_patched_SOURCE_DIR}/include" CACHE PATH "mpv include directory" FORCE)
            set(mpv_LIBRARY "${mpv_patched_SOURCE_DIR}/lib/libmpv.so" CACHE FILEPATH "mpv library" FORCE)
        else()
            message(STATUS "Prebuilt patched libmpv SDK not available for this platform; falling back to system search.")
        endif()
    endif()
endif()

if(NOT mpv_LIBRARY OR NOT mpv_INCLUDE_DIR)
    set(_mpv_system_hints
        ${_mpv_custom_hints}
        ${PC_MPV_LIBDIR}
        ${PC_MPV_LIBRARY_DIRS}
    )
    find_library(
        mpv_LIBRARY
        NAMES mpv
        HINTS ${_mpv_system_hints}
        PATH_SUFFIXES lib lib64
    )
    find_path(
        mpv_INCLUDE_DIR
        NAMES mpv/client.h mpv/render_gl.h
        HINTS ${_mpv_system_hints}
        PATH_SUFFIXES include
    )
endif()

find_package_handle_standard_args(
    mpv
    REQUIRED_VARS mpv_LIBRARY mpv_INCLUDE_DIR
)

if(mpv_FOUND)
    mark_as_advanced(mpv_LIBRARY)
    mark_as_advanced(mpv_INCLUDE_DIR)
endif()

if(mpv_FOUND AND NOT TARGET mpv::mpv)
    add_library(mpv::mpv UNKNOWN IMPORTED GLOBAL)
    get_filename_component(_mpv_lib_dir "${mpv_LIBRARY}" DIRECTORY)
    set_target_properties(
        mpv::mpv PROPERTIES
        IMPORTED_LOCATION "${mpv_LIBRARY}"
        INTERFACE_INCLUDE_DIRECTORIES "${mpv_INCLUDE_DIR}"
    )
    if(UNIX AND NOT APPLE)
        set_target_properties(
            mpv::mpv PROPERTIES
            INTERFACE_LINK_OPTIONS "-Wl,-rpath,${_mpv_lib_dir}"
        )
    endif()
endif()
