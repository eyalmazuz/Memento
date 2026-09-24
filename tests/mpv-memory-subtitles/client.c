////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <mpv/client.h>

/**
 * @brief Wait for a file load event.
 *
 * @param mpv The initialized client handle.
 * @return true if a file loaded within ten seconds.
 */
static bool wait_file_loaded(mpv_handle *mpv)
{
    for (int i = 0; i < 100; ++i)
    {
        mpv_event *event = mpv_wait_event(mpv, 0.1);
        if (event->event_id == MPV_EVENT_FILE_LOADED)
        {
            return true;
        }
        if (event->event_id == MPV_EVENT_SHUTDOWN)
        {
            return false;
        }
    }
    return false;
}

/**
 * @brief Wait for an observed property-change event.
 *
 * @param mpv The initialized client handle.
 * @param name The observed property name.
 * @return true if the event arrived within five seconds.
 */
static bool wait_property_change(mpv_handle *mpv, const char *name)
{
    static int call_count = 0;
    ++call_count;
    for (int i = 0; i < 50; ++i)
    {
        mpv_event *event = mpv_wait_event(mpv, 0.1);
        if (event->event_id != MPV_EVENT_PROPERTY_CHANGE ||
            event->data == NULL)
        {
            continue;
        }

        mpv_event_property *property = event->data;
        if (property->name != NULL && strcmp(property->name, name) == 0)
        {
            return true;
        }
    }
    fprintf(stderr, "Timed out waiting for property event %d: %s\n",
        call_count, name);
    return false;
}

/**
 * @brief Wait until a property returns the expected subtitle text.
 *
 * @param mpv The initialized client handle.
 * @param expected The expected text.
 * @return true if the property matched within five seconds.
 */
static bool wait_sub_text(mpv_handle *mpv, const char *expected)
{
    for (int i = 0; i < 50; ++i)
    {
        char *text = mpv_get_property_string(mpv, "sub-text");
        bool matches = text != NULL && strcmp(text, expected) == 0;
        mpv_free(text);
        if (matches)
        {
            return true;
        }
        mpv_wait_event(mpv, 0.1);
    }
    return false;
}

/**
 * @brief Execute a command that should return an integer ID.
 *
 * @param mpv The initialized client handle.
 * @param command The null-terminated command arguments.
 * @param id Receives the returned ID.
 * @return true if the command returned an integer ID.
 */
static bool command_id(
    mpv_handle *mpv, const char *command[], int64_t *id)
{
    mpv_node result = {0};
    int error = mpv_command_ret(mpv, command, &result);
    bool success = error >= 0 && result.format == MPV_FORMAT_INT64;
    if (success)
    {
        *id = result.u.int64;
    }
    else
    {
        fprintf(stderr, "Command failed or returned no ID: %s\n",
            error < 0 ? mpv_error_string(error) : "wrong result format");
    }
    mpv_free_node_contents(&result);
    return success;
}

/**
 * @brief Emit one deterministic segment from the fake ASR producer.
 *
 * @param mpv The initialized client handle.
 * @param track_id The destination subtitle track.
 * @param start The segment start time in seconds.
 * @param end The segment end time in seconds.
 * @param text The segment text.
 * @param cue_id Receives the cue ID assigned by mpv.
 * @return true if mpv accepted the segment.
 */
static bool fake_asr_segment(
    mpv_handle *mpv,
    int64_t track_id,
    double start,
    double end,
    const char *text,
    int64_t *cue_id)
{
    char track_arg[32];
    char start_arg[32];
    char end_arg[32];
    snprintf(track_arg, sizeof(track_arg), "%lld", (long long)track_id);
    snprintf(start_arg, sizeof(start_arg), "%.3f", start);
    snprintf(end_arg, sizeof(end_arg), "%.3f", end);
    const char *command[] = {
        "add-subtitle", track_arg, start_arg, end_arg, text, NULL
    };
    return command_id(mpv, command, cue_id);
}

/**
 * @brief Execute a command that should succeed without a result.
 *
 * @param mpv The initialized client handle.
 * @param command The null-terminated command arguments.
 * @return true if the command succeeded.
 */
static bool command_ok(mpv_handle *mpv, const char *command[])
{
    int error = mpv_command(mpv, command);
    if (error < 0)
    {
        fprintf(stderr, "Command failed: %s\n", mpv_error_string(error));
        return false;
    }
    return true;
}

/**
 * @brief Get the number of cues exposed by sub-lines.
 *
 * @param mpv The initialized client handle.
 * @return The cue count, or -1 if the property has an unexpected format.
 */
static int line_count(mpv_handle *mpv)
{
    mpv_node lines = {0};
    int error = mpv_get_property(mpv, "sub-lines", MPV_FORMAT_NODE, &lines);
    if (error < 0 || lines.format != MPV_FORMAT_NODE_ARRAY ||
        lines.u.list == NULL)
    {
        fprintf(stderr, "sub-lines query failed: %s\n",
            error < 0 ? mpv_error_string(error) : "unexpected node format");
        mpv_free_node_contents(&lines);
        return -1;
    }

    int count = lines.u.list->num;
    mpv_free_node_contents(&lines);
    return count;
}

/**
 * @brief Check the sole cue returned by sub-lines.
 *
 * @param mpv The initialized client handle.
 * @param expected_id The expected cue ID.
 * @param expected_text The expected cue text.
 * @return true if ID, text, and times match.
 */
static bool check_single_line(
    mpv_handle *mpv,
    int64_t expected_id,
    const char *expected_text,
    double expected_start,
    double expected_end)
{
    mpv_node lines = {0};
    int error = mpv_get_property(mpv, "sub-lines", MPV_FORMAT_NODE, &lines);
    bool success = false;
    if (error < 0 || lines.format != MPV_FORMAT_NODE_ARRAY ||
        lines.u.list == NULL || lines.u.list->num != 1)
    {
        goto done;
    }

    mpv_node *entry = &lines.u.list->values[0];
    if (entry->format != MPV_FORMAT_NODE_MAP || entry->u.list == NULL)
    {
        goto done;
    }

    bool has_id = false;
    bool has_text = false;
    bool has_start = false;
    bool has_end = false;
    int64_t id = 0;
    const char *text = NULL;
    double start = 0.0;
    double end = 0.0;
    mpv_node_list *map = entry->u.list;
    for (int i = 0; i < map->num; ++i)
    {
        const char *key = map->keys[i];
        mpv_node *value = &map->values[i];
        if (key == NULL)
        {
            continue;
        }

        if (strcmp(key, "id") == 0 && value->format == MPV_FORMAT_INT64)
        {
            id = value->u.int64;
            has_id = true;
        }
        else if (strcmp(key, "text") == 0 &&
            value->format == MPV_FORMAT_STRING)
        {
            text = value->u.string;
            has_text = text != NULL;
        }
        else if (strcmp(key, "start") == 0 &&
            value->format == MPV_FORMAT_DOUBLE)
        {
            start = value->u.double_;
            has_start = true;
        }
        else if (strcmp(key, "end") == 0 &&
            value->format == MPV_FORMAT_DOUBLE)
        {
            end = value->u.double_;
            has_end = true;
        }
    }

    success = has_id && has_text && has_start && has_end &&
        id == expected_id && strcmp(text, expected_text) == 0 &&
        fabs(start - expected_start) < 0.0001 &&
        fabs(end - expected_end) < 0.0001;

done:
    if (!success)
    {
        fprintf(stderr, "sub-lines cue did not match expected map\n");
    }
    mpv_free_node_contents(&lines);
    return success;
}

/**
 * @brief Check whether a subtitle track ID remains in track-list.
 *
 * @param mpv The initialized client handle.
 * @param track_id The track ID to find.
 * @param present Receives whether the ID is present.
 * @return true if track-list had the expected node format.
 */
static bool track_present(
    mpv_handle *mpv, int64_t track_id, bool *present)
{
    mpv_node tracks = {0};
    int error = mpv_get_property(mpv, "track-list", MPV_FORMAT_NODE, &tracks);
    if (error < 0 || tracks.format != MPV_FORMAT_NODE_ARRAY ||
        tracks.u.list == NULL)
    {
        fprintf(stderr, "track-list query failed: %s\n",
            error < 0 ? mpv_error_string(error) : "unexpected node format");
        mpv_free_node_contents(&tracks);
        return false;
    }

    *present = false;
    for (int i = 0; i < tracks.u.list->num; ++i)
    {
        mpv_node *entry = &tracks.u.list->values[i];
        if (entry->format != MPV_FORMAT_NODE_MAP || entry->u.list == NULL)
        {
            continue;
        }
        mpv_node_list *map = entry->u.list;
        bool is_subtitle = false;
        int64_t id = 0;
        for (int j = 0; j < map->num; ++j)
        {
            const char *key = map->keys[j];
            mpv_node *value = &map->values[j];
            if (key == NULL)
            {
                continue;
            }
            if (strcmp(key, "id") == 0 &&
                value->format == MPV_FORMAT_INT64)
            {
                id = value->u.int64;
            }
            else if (strcmp(key, "type") == 0 &&
                value->format == MPV_FORMAT_STRING &&
                value->u.string != NULL &&
                strcmp(value->u.string, "sub") == 0)
            {
                is_subtitle = true;
            }
        }
        if (is_subtitle && id == track_id)
        {
            *present = true;
        }
    }

    mpv_free_node_contents(&tracks);
    return true;
}

/**
 * @brief Wait for a track ID to reach its expected presence state.
 *
 * @param mpv The initialized client handle.
 * @param track_id The track ID to find.
 * @param expected The expected presence state.
 * @return true if track-list reaches the expected state.
 */
static bool wait_track_state(
    mpv_handle *mpv, int64_t track_id, bool expected)
{
    for (int i = 0; i < 50; ++i)
    {
        bool present = false;
        if (!track_present(mpv, track_id, &present))
        {
            return false;
        }
        if (present == expected)
        {
            return true;
        }
        mpv_wait_event(mpv, 0.1);
    }
    bool still_present = false;
    track_present(mpv, track_id, &still_present);
    fprintf(stderr, "Track %lld expected present=%d, actual=%d\n",
        (long long)track_id, expected, still_present);
    return false;
}

/**
 * @brief Run in-memory subtitle lifecycle and property-event checks.
 *
 * @param argc The argument count.
 * @param argv The media path is passed as the first argument.
 * @return EXIT_SUCCESS if all checks pass.
 */
int main(int argc, char **argv)
{
    if (argc != 2)
    {
        fprintf(stderr, "usage: %s <media-file>\n", argv[0]);
        return EXIT_FAILURE;
    }

    mpv_handle *mpv = mpv_create();
    if (mpv == NULL)
    {
        fprintf(stderr, "mpv_create failed\n");
        return EXIT_FAILURE;
    }

    int exit_code = EXIT_FAILURE;
    if (mpv_set_option_string(mpv, "config", "no") < 0 ||
        mpv_set_option_string(mpv, "terminal", "no") < 0 ||
        mpv_set_option_string(mpv, "vo", "null") < 0 ||
        mpv_set_option_string(mpv, "ao", "null") < 0 ||
        mpv_set_option_string(mpv, "pause", "yes") < 0 ||
        mpv_initialize(mpv) < 0)
    {
        fprintf(stderr, "mpv initialization failed\n");
        goto done;
    }

    if (mpv_observe_property(mpv, 1, "track-list", MPV_FORMAT_NODE) < 0 ||
        mpv_observe_property(mpv, 2, "sub-lines", MPV_FORMAT_NODE) < 0 ||
        mpv_observe_property(mpv, 3, "sub-text", MPV_FORMAT_STRING) < 0)
    {
        fprintf(stderr, "property observation setup failed\n");
        goto done;
    }

    const char *load[] = {"loadfile", argv[1], "replace", NULL};
    if (!command_ok(mpv, load) || !wait_file_loaded(mpv))
    {
        fprintf(stderr, "media did not reach FILE_LOADED\n");
        goto done;
    }

    const char *create[] = {"sub-create", "Memory test", "ja", NULL};
    int64_t track_id = 0;
    if (!command_id(mpv, create, &track_id) || track_id <= 0 ||
        !wait_property_change(mpv, "track-list"))
    {
        goto done;
    }
    while (mpv_wait_event(mpv, 0)->event_id != MPV_EVENT_NONE)
    {
    }

    char track_arg[32];
    snprintf(track_arg, sizeof(track_arg), "%lld", (long long)track_id);
    int64_t cue_id = 0;
    if (!fake_asr_segment(
            mpv, track_id, 1.0, 2.5, "first\nline", &cue_id) ||
        cue_id <= 0 ||
        !wait_property_change(mpv, "sub-lines") ||
        line_count(mpv) != 1 ||
        !check_single_line(mpv, cue_id, "first\nline", 1.0, 2.5))
    {
        goto done;
    }

    const char *edit[] = {
        "edit-subtitle", track_arg, "", "0", "2.5", "edited", NULL
    };
    char cue_arg[32];
    snprintf(cue_arg, sizeof(cue_arg), "%lld", (long long)cue_id);
    edit[2] = cue_arg;
    if (!command_ok(mpv, edit) ||
        !wait_property_change(mpv, "sub-lines") ||
        !wait_property_change(mpv, "sub-text") ||
        !check_single_line(mpv, cue_id, "edited", 0.0, 2.5) ||
        !wait_sub_text(mpv, "edited"))
    {
        goto done;
    }

    const char *retime[] = {
        "edit-subtitle", track_arg, cue_arg, "1.0", "2.5", "edited", NULL
    };
    if (!command_ok(mpv, retime) ||
        !wait_property_change(mpv, "sub-lines") ||
        !wait_property_change(mpv, "sub-text") ||
        !wait_sub_text(mpv, "") ||
        !check_single_line(mpv, cue_id, "edited", 1.0, 2.5) ||
        mpv_set_property_string(mpv, "sub-speed", "0.5") < 0 ||
        mpv_set_property_string(mpv, "sub-delay", "0.25") < 0 ||
        !command_ok(mpv, (const char *[]) {
            "seek", "1.0", "absolute+exact", NULL}) ||
        !wait_sub_text(mpv, "edited"))
    {
        goto done;
    }
    for (int i = 0; i < 50; ++i)
    {
        if (mpv_wait_event(mpv, 0.1)->event_id == MPV_EVENT_PLAYBACK_RESTART)
        {
            break;
        }
    }
    double timing_start = 0.0;
    double timing_end = 0.0;
    if (mpv_get_property(
            mpv, "sub-start", MPV_FORMAT_DOUBLE, &timing_start) < 0 ||
        mpv_get_property(
            mpv, "sub-end", MPV_FORMAT_DOUBLE, &timing_end) < 0 ||
        fabs(timing_start - 1.0) >= 0.0001 ||
        fabs(timing_end - 2.5) >= 0.0001 ||
        !check_single_line(mpv, cue_id, "edited", 1.0, 2.5) ||
        mpv_set_property_string(mpv, "sub-speed", "1") < 0 ||
        mpv_set_property_string(mpv, "sub-delay", "0") < 0)
    {
        goto done;
    }

    const char *remove_cue[] = {
        "remove-subtitle", track_arg, cue_arg, NULL
    };
    if (!command_ok(mpv, remove_cue) ||
        !wait_property_change(mpv, "sub-lines") || line_count(mpv) != 0)
    {
        goto done;
    }

    int64_t ignored_id = 0;
    if (!fake_asr_segment(mpv, track_id, 0.0, 2.5, "one", &ignored_id) ||
        !fake_asr_segment(mpv, track_id, 0.0, 2.5, "two", &ignored_id) ||
        line_count(mpv) != 2)
    {
        goto done;
    }

    const char *clear[] = {"clear-subtitles", track_arg, NULL};
    if (!command_ok(mpv, clear) || line_count(mpv) != 0)
    {
        goto done;
    }

    const char *remove_track[] = {"sub-remove", track_arg, NULL};
    if (!command_ok(mpv, remove_track))
    {
        goto done;
    }
    if (!wait_property_change(mpv, "track-list"))
    {
        fprintf(stderr, "sub-remove did not emit track-list event\n");
        goto done;
    }
    if (!wait_track_state(mpv, track_id, false))
    {
        fprintf(stderr, "sub-remove did not remove the created track\n");
        goto done;
    }

    if (!command_id(mpv, create, &track_id) || track_id <= 0 ||
        !wait_property_change(mpv, "track-list") ||
        !command_ok(mpv, load) || !wait_file_loaded(mpv) ||
        !wait_track_state(mpv, track_id, false))
    {
        fprintf(stderr, "track did not follow file lifecycle\n");
        goto done;
    }

    puts("PASS: memory subtitle IDs, cues, properties, events, and lifecycle");
    exit_code = EXIT_SUCCESS;

done:
    mpv_terminate_destroy(mpv);
    return exit_code;
}
