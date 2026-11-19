#include <stdarg.h>
#include <stddef.h>

typedef struct ass_library ASS_Library;

extern void ass_set_message_cb(
    ASS_Library *priv,
    void (*msg_cb)(int level, const char *fmt, va_list args, void *data),
    void *data
);

static void memento_ass_silent_message_cb(
    int level,
    const char *fmt,
    va_list args,
    void *data
) {
    (void)level;
    (void)fmt;
    (void)args;
    (void)data;
}

void memento_ass_silence_library(ASS_Library *library) {
    if (library != NULL) {
        ass_set_message_cb(library, memento_ass_silent_message_cb, NULL);
    }
}
