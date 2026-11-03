// JSON, the Go twin of json.fen.
package main

import (
	"fmt"
	"strconv"
	"strings"
)

type Json interface{}

type Null struct{}
type Flag bool
type Number int64
type Text string
type Items []Json
type Fields struct {
	names  []string
	values []Json
}

type Reader struct {
	text []byte
	at   int
}

func (r *Reader) skipSpace() {
	for r.at < len(r.text) && r.text[r.at] == ' ' {
		r.at++
	}
}

func (r *Reader) expectText() string {
	r.at++
	start := r.at
	for r.text[r.at] != '"' {
		r.at++
	}
	found := string(r.text[start:r.at])
	r.at++
	return found
}

func (r *Reader) number() int64 {
	var value int64
	for r.at < len(r.text) && r.text[r.at] >= '0' && r.text[r.at] <= '9' {
		value = value*10 + int64(r.text[r.at]-'0')
		r.at++
	}
	return value
}

func (r *Reader) items() Json {
	r.at++
	values := []Json{}
	r.skipSpace()
	for r.text[r.at] != ']' {
		values = append(values, r.value())
		r.skipSpace()
		if r.text[r.at] == ',' {
			r.at++
			r.skipSpace()
		}
	}
	r.at++
	return Items(values)
}

func (r *Reader) fields() Json {
	r.at++
	names, values := []string{}, []Json{}
	r.skipSpace()
	for r.text[r.at] != '}' {
		names = append(names, r.expectText())
		r.skipSpace()
		r.at++
		values = append(values, r.value())
		r.skipSpace()
		if r.text[r.at] == ',' {
			r.at++
			r.skipSpace()
		}
	}
	r.at++
	return &Fields{names, values}
}

func (r *Reader) value() Json {
	r.skipSpace()
	switch r.text[r.at] {
	case '{':
		return r.fields()
	case '[':
		return r.items()
	case '"':
		return Text(r.expectText())
	case 't':
		r.at += 4
		return Flag(true)
	case 'f':
		r.at += 5
		return Flag(false)
	case 'n':
		r.at += 4
		return Null{}
	default:
		return Number(r.number())
	}
}

func printed(value Json, out *strings.Builder) {
	switch v := value.(type) {
	case Null:
		out.WriteString("null")
	case Flag:
		if v {
			out.WriteString("true")
		} else {
			out.WriteString("false")
		}
	case Number:
		out.WriteString(strconv.FormatInt(int64(v), 10))
	case Text:
		out.WriteByte('"')
		out.WriteString(string(v))
		out.WriteByte('"')
	case Items:
		out.WriteByte('[')
		for at, item := range v {
			if at > 0 {
				out.WriteByte(',')
			}
			printed(item, out)
		}
		out.WriteByte(']')
	case *Fields:
		out.WriteByte('{')
		for at, name := range v.names {
			if at > 0 {
				out.WriteByte(',')
			}
			out.WriteByte('"')
			out.WriteString(name)
			out.WriteString("\":")
			printed(v.values[at], out)
		}
		out.WriteByte('}')
	}
}

func document(records int64) string {
	var text strings.Builder
	text.WriteString("[")
	for i := int64(0); i < records; i++ {
		if i > 0 {
			text.WriteString(", ")
		}
		active := "false"
		if i%2 == 0 {
			active = "true"
		}
		text.WriteString(fmt.Sprintf("{\"id\": %d, \"name\": \"fox%d\", \"active\": %s, \"parent\": null, ", i, i, active))
		text.WriteString(fmt.Sprintf("\"tags\": [\"a\", \"b\", %d], \"at\": {\"x\": %d, \"y\": %d}}", i%7, i*3, i*5))
	}
	text.WriteByte(']')
	return text.String()
}

func main() {
	text := document(300)
	total := 0
	for range 40 {
		reader := Reader{text: []byte(text), at: 0}
		value := reader.value()
		var out strings.Builder
		printed(value, &out)
		total += out.Len()
	}
	fmt.Printf("%d %d\n", len(text), total)
}
