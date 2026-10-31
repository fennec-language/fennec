// The spike's runtime: lists, printing and failure, enough for the kernels it lowers.
#include <pthread.h>
#include <signal.h>
#include <unistd.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    int64_t len;
    int64_t cap;
    char *items;
} List;

uint64_t fen_stack_limit = 0;
static int failed_kind = 0;
static int64_t failed_span = 0;

// The stack's guard page, when nothing checks on each call: running into it ends the program.
static void on_fault(int sig) {
    (void)sig;
    static const char text[] = "failed: the stack overflowed\n";
    fflush(stdout);
    (void)!write(2, text, sizeof text - 1);
    _exit(1);
}

void fen_init(void) {
    static char alternate[1 << 16];
    stack_t stack = {.ss_sp = alternate, .ss_size = sizeof alternate, .ss_flags = 0};
    sigaltstack(&stack, NULL);
    struct sigaction action = {0};
    action.sa_handler = on_fault;
    action.sa_flags = SA_ONSTACK;
    sigaction(SIGSEGV, &action, NULL);
    sigaction(SIGBUS, &action, NULL);
    pthread_t self = pthread_self();
    fen_stack_limit = (uint64_t)pthread_get_stackaddr_np(self) - pthread_get_stacksize_np(self) + 65536;
    static char buffer[1 << 16];
    setvbuf(stdout, buffer, _IOFBF, sizeof buffer);
}

int fen_finish(bool failed) {
    fflush(stdout);
    if (failed) {
        fprintf(stderr, "failed: kind %d at %lld\n", failed_kind, (long long)failed_span);
        return 1;
    }
    return 0;
}

__attribute__((cold)) void fen_fail(int kind, int64_t span) {
    if (!failed_kind) {
        failed_kind = kind;
        failed_span = span;
    }
}

__attribute__((cold, noreturn)) void fen_die(int kind, int64_t span) {
    fflush(stdout);
    fprintf(stderr, "failed: kind %d at %lld\n", kind, (long long)span);
    exit(1);
}

List *fen_list_new(int64_t size, int64_t cap) {
    List *list = malloc(sizeof(List));
    list->len = 0;
    list->cap = cap;
    list->items = cap ? malloc((size_t)(size * cap)) : NULL;
    return list;
}

void fen_list_grow(List *list, int64_t size) {
    list->cap = list->cap ? list->cap * 2 : 4;
    list->items = realloc(list->items, (size_t)(size * list->cap));
}

void fen_list_reserve(List *list, int64_t size, int64_t count) {
    if (count > list->cap) {
        list->cap = count;
        list->items = realloc(list->items, (size_t)(size * list->cap));
    }
}

void fen_print_int(int64_t value) { printf("%lld", (long long)value); }
void fen_print_float(double value) { printf("%g", value); }
void fen_print_bool(bool value) { fputs(value ? "True" : "False", stdout); }
void fen_print_str(const char *value) { fputs(value, stdout); }
void fen_print_sep(void) { putchar(' '); }
void fen_print_nl(void) { putchar('\n'); }

static char fmt_buffer[4096];
static size_t fmt_len;

void fen_fmt_begin(void) { fmt_len = 0; }
void fen_fmt_text(const char *text) {
    fmt_len += (size_t)snprintf(fmt_buffer + fmt_len, sizeof fmt_buffer - fmt_len, "%s", text);
}
void fen_fmt_int(int64_t value, const char *spec) {
    (void)spec;
    fmt_len += (size_t)snprintf(fmt_buffer + fmt_len, sizeof fmt_buffer - fmt_len, "%lld", (long long)value);
}
void fen_fmt_float(double value, const char *spec) {
    char format[32];
    snprintf(format, sizeof format, "%%%s", *spec ? spec : "g");
    fmt_len += (size_t)snprintf(fmt_buffer + fmt_len, sizeof fmt_buffer - fmt_len, format, value);
}
char *fen_fmt_end(void) { return strdup(fmt_buffer); }
