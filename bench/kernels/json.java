// JSON, the Java twin of json.fen.

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

public class json {
    sealed interface Json permits Null, Flag, Number, Text, Items, Fields {}

    record Null() implements Json {}

    record Flag(boolean flag) implements Json {}

    record Number(long number) implements Json {}

    record Text(String text) implements Json {}

    record Items(List<Json> values) implements Json {}

    record Fields(List<String> names, List<Json> values) implements Json {}

    static final class Reader {
        final byte[] text;
        int at;

        Reader(byte[] text) {
            this.text = text;
            this.at = 0;
        }

        void skipSpace() {
            while (at < text.length && text[at] == ' ') {
                at++;
            }
        }

        String expectText() {
            at++;
            int start = at;
            while (text[at] != '"') {
                at++;
            }
            String found = new String(text, start, at - start, StandardCharsets.UTF_8);
            at++;
            return found;
        }

        long number() {
            long value = 0;
            while (at < text.length && text[at] >= '0' && text[at] <= '9') {
                value = value * 10 + (text[at] - '0');
                at++;
            }
            return value;
        }

        Json items() {
            at++;
            List<Json> values = new ArrayList<>();
            skipSpace();
            while (text[at] != ']') {
                values.add(value());
                skipSpace();
                if (text[at] == ',') {
                    at++;
                    skipSpace();
                }
            }
            at++;
            return new Items(values);
        }

        Json fields() {
            at++;
            List<String> names = new ArrayList<>();
            List<Json> values = new ArrayList<>();
            skipSpace();
            while (text[at] != '}') {
                names.add(expectText());
                skipSpace();
                at++;
                values.add(value());
                skipSpace();
                if (text[at] == ',') {
                    at++;
                    skipSpace();
                }
            }
            at++;
            return new Fields(names, values);
        }

        Json value() {
            skipSpace();
            switch (text[at]) {
                case '{':
                    return fields();
                case '[':
                    return items();
                case '"':
                    return new Text(expectText());
                case 't':
                    at += 4;
                    return new Flag(true);
                case 'f':
                    at += 5;
                    return new Flag(false);
                case 'n':
                    at += 4;
                    return new Null();
                default:
                    return new Number(number());
            }
        }
    }

    static void printed(Json value, StringBuilder out) {
        switch (value) {
            case Null n -> out.append("null");
            case Flag f -> out.append(f.flag() ? "true" : "false");
            case Number n -> out.append(n.number());
            case Text t -> out.append('"').append(t.text()).append('"');
            case Items items -> {
                out.append('[');
                List<Json> values = items.values();
                for (int at = 0; at < values.size(); at++) {
                    if (at > 0) {
                        out.append(',');
                    }
                    printed(values.get(at), out);
                }
                out.append(']');
            }
            case Fields fields -> {
                out.append('{');
                List<String> names = fields.names();
                for (int at = 0; at < names.size(); at++) {
                    if (at > 0) {
                        out.append(',');
                    }
                    out.append('"').append(names.get(at)).append("\":");
                    printed(fields.values().get(at), out);
                }
                out.append('}');
            }
        }
    }

    static String document(long records) {
        StringBuilder text = new StringBuilder("[");
        for (long i = 0; i < records; i++) {
            if (i > 0) {
                text.append(", ");
            }
            String active = i % 2 == 0 ? "true" : "false";
            text.append(String.format("{\"id\": %d, \"name\": \"fox%d\", \"active\": %s, \"parent\": null, ", i, i, active));
            text.append(String.format("\"tags\": [\"a\", \"b\", %d], \"at\": {\"x\": %d, \"y\": %d}}", i % 7, i * 3, i * 5));
        }
        text.append(']');
        return text.toString();
    }

    public static void main(String[] args) {
        String text = document(300);
        byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        long total = 0;
        for (int round = 0; round < 40; round++) {
            Reader reader = new Reader(bytes);
            Json value = reader.value();
            StringBuilder out = new StringBuilder();
            printed(value, out);
            total += out.length();
        }
        System.out.println(bytes.length + " " + total);
    }
}
