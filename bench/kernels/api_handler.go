// A JSON API's request handler, the Go twin of api_handler.fen: requests split from their text
// with strings.Cut, bodies decoded into structs by encoding/json with unknown fields and anything
// after the object refused, then each struct's rules checked, users in a map of pointers changed
// in place, orders in a map of slices, and responses written into byte slices. encoding/json's
// Marshal writes no space after a colon or a comma, so responses are appended by hand, to print
// the same bytes as the Fennec and Rust versions. Unlike them, encoding/json matches a field's
// name in any case and keeps the last of a repeated field; the corpus sends neither.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"maps"
	"os"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	requests   = 50_000
	corpusSize = 4_096
	dayLength  = 2_500
	seen       = 4_000
	shown      = 20
)

var items = [...]string{"arrow", "potion", "bone", "herb", "gem", "scroll", "key", "lantern"}

// What a client sends to make a user. A field left out stays nil, and is refused.
type NewUser struct {
	Name  *string `json:"name"`
	Age   *int64  `json:"age"`
	Email *string `json:"email"`
}

func (made *NewUser) valid() bool {
	return made.Name != nil && made.Age != nil && made.Email != nil &&
		*made.Name != "" && utf8.RuneCountInString(*made.Name) <= 40 &&
		*made.Age >= 13 && *made.Age <= 120 &&
		strings.Contains(*made.Email, "@")
}

// What a client sends to place an order.
type NewOrder struct {
	User     *int64  `json:"user"`
	Item     *string `json:"item"`
	Quantity *int64  `json:"quantity"`
	Price    *int64  `json:"price"`
}

func (made *NewOrder) valid() bool {
	return made.User != nil && made.Item != nil && made.Quantity != nil && made.Price != nil &&
		*made.User >= 0 && *made.Item != "" &&
		*made.Quantity >= 1 && *made.Quantity <= 100 &&
		*made.Price >= 0
}

// What a client sends to change a user's age.
type AgeChange struct {
	Age *int64 `json:"age"`
}

func (change *AgeChange) valid() bool {
	return change.Age != nil && *change.Age >= 13 && *change.Age <= 120
}

type User struct {
	id     int64
	name   string
	age    int64
	email  string
	spent  int64
	orders int64
}

type Order struct {
	id       int64
	item     string
	quantity int64
	price    int64
}

type Response struct {
	status int64
	body   string
}

// A request's head and body, split apart.
type Request struct {
	method  string
	path    string
	headers map[string]string
	body    string
}

type Service struct {
	users     map[int64]*User
	orders    map[int64][]Order
	cache     map[int64]string
	hits      map[string]int64
	stats     string
	nextUser  int64
	nextOrder int64
}

// decoded reads a client's JSON object into v: a field v doesn't have, or anything after the
// object, is refused.
func decoded(body string, v any) bool {
	decoder := json.NewDecoder(strings.NewReader(body))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(v); err != nil {
		return false
	}
	_, err := decoder.Token()
	return err == io.EOF
}

// appendQuoted writes text as a JSON string.
func appendQuoted(out []byte, text string) []byte {
	out = append(out, '"')
	for _, c := range text {
		switch {
		case c == '"':
			out = append(out, `\"`...)
		case c == '\\':
			out = append(out, `\\`...)
		case c == '\n':
			out = append(out, `\n`...)
		case c == '\r':
			out = append(out, `\r`...)
		case c == '\t':
			out = append(out, `\t`...)
		case c < 0x20:
			out = fmt.Appendf(out, `\u%04x`, c)
		default:
			out = utf8.AppendRune(out, c)
		}
	}
	return append(out, '"')
}

func (user *User) dump() string {
	out := make([]byte, 0, 128)
	out = append(out, `{"id": `...)
	out = strconv.AppendInt(out, user.id, 10)
	out = append(out, `, "name": `...)
	out = appendQuoted(out, user.name)
	out = append(out, `, "age": `...)
	out = strconv.AppendInt(out, user.age, 10)
	out = append(out, `, "email": `...)
	out = appendQuoted(out, user.email)
	out = append(out, `, "spent": `...)
	out = strconv.AppendInt(out, user.spent, 10)
	out = append(out, `, "orders": `...)
	out = strconv.AppendInt(out, user.orders, 10)
	out = append(out, '}')
	return string(out)
}

func (order *Order) appendTo(out []byte) []byte {
	out = append(out, `{"id": `...)
	out = strconv.AppendInt(out, order.id, 10)
	out = append(out, `, "item": `...)
	out = appendQuoted(out, order.item)
	out = append(out, `, "quantity": `...)
	out = strconv.AppendInt(out, order.quantity, 10)
	out = append(out, `, "price": `...)
	out = strconv.AppendInt(out, order.price, 10)
	return append(out, '}')
}

// The next state of a small linear congruential generator, the same on every machine.
func next(seed int64) int64 {
	return (seed*1_103_515_245 + 12_345) % 2_147_483_648
}

// A number from 0 up to top taken from a generator's state.
func below(seed, top int64) int64 {
	return seed / 65_536 % top
}

// A request as a client writes it: a request line, headers and, with a body, its length.
func written(method, path, body string) string {
	var text strings.Builder
	fmt.Fprintf(&text, "%s %s HTTP/1.1\r\nHost: api.den.example\r\n", method, path)
	if body != "" {
		text.WriteString("Content-Type: application/json\r\n")
		fmt.Fprintf(&text, "Content-Length: %d\r\n", len(body))
	}
	text.WriteString("\r\n")
	text.WriteString(body)
	return text.String()
}

// The corpus: every kind of request, a few of them broken the ways clients break them.
func corpus(seed int64) []string {
	state := seed
	made := make([]string, 0, corpusSize)
	for range corpusSize {
		state = next(state)
		kind := below(state, 100)
		state = next(state)
		who := below(state, seen)
		state = next(state)
		amount := below(state, 130)
		var request string
		switch {
		case kind < 30:
			// Every twentieth new user is too young, and so refused.
			age := 13 + amount%90
			if who%20 == 0 {
				age = 9
			}
			body := fmt.Sprintf(`{"name": "fox-%d", "age": %d, "email": "fox-%d@den.example"}`, who, age, who)
			request = written("POST", "/users", body)
		case kind < 55:
			request = written("GET", fmt.Sprintf("/users/%d", who), "")
		case kind < 80:
			item := items[amount%int64(len(items))]
			body := fmt.Sprintf(`{"user": %d, "item": "%s", "quantity": %d, "price": %d}`, who, item, amount%12, amount*3)
			request = written("POST", "/orders", body)
		case kind < 90:
			request = written("GET", fmt.Sprintf("/orders/%d", who), "")
		case kind < 95:
			request = written("PATCH", fmt.Sprintf("/users/%d", who), fmt.Sprintf(`{"age": %d}`, amount))
		case kind < 97:
			request = written("GET", "/stats", "")
		case kind < 98:
			request = written("POST", "/users", `{"name": "fox", "age": }`)
		case kind < 99:
			request = written("DELETE", fmt.Sprintf("/users/%d", who), "")
		default:
			request = written("GET", fmt.Sprintf("/admin/%d", who), "")
		}
		made = append(made, request)
	}
	return made
}

// Splits a request's text into its head's parts and its body, or gives false when it isn't a
// request.
func parsed(text string) (Request, bool) {
	head, body, found := strings.Cut(text, "\r\n\r\n")
	if !found {
		return Request{}, false
	}
	lines := strings.Split(head, "\r\n")
	words := strings.Split(lines[0], " ")
	if len(words) != 3 {
		return Request{}, false
	}
	headers := make(map[string]string, len(lines)-1)
	for _, line := range lines[1:] {
		name, value, found := strings.Cut(line, ": ")
		if !found {
			return Request{}, false
		}
		headers[name] = value
	}
	return Request{method: words[0], path: words[1], headers: headers, body: body}, true
}

// Whether request says its body is JSON and gives its length right.
func checkedBody(request *Request) bool {
	says, err := strconv.ParseUint(request.headers["Content-Length"], 10, 64)
	return err == nil && request.headers["Content-Type"] == "application/json" && says == uint64(len(request.body))
}

func failed(status int64, message string) Response {
	return Response{status, `{"error": "` + message + `"}`}
}

func (service *Service) createUser(request *Request) Response {
	if !checkedBody(request) {
		return failed(415, "a JSON body is needed")
	}
	var made NewUser
	if !decoded(request.body, &made) || !made.valid() {
		return failed(400, "the body isn't a user")
	}
	id := service.nextUser
	service.nextUser++
	user := &User{id: id, name: *made.Name, age: *made.Age, email: *made.Email}
	service.users[id] = user
	return Response{201, user.dump()}
}

func (service *Service) showUser(id int64) Response {
	if cached, found := service.cache[id]; found {
		return Response{200, cached}
	}
	user, found := service.users[id]
	if !found {
		return failed(404, fmt.Sprintf("no user %d", id))
	}
	text := user.dump()
	service.cache[id] = text
	return Response{200, text}
}

func (service *Service) changeAge(id int64, request *Request) Response {
	if !checkedBody(request) {
		return failed(415, "a JSON body is needed")
	}
	var change AgeChange
	if !decoded(request.body, &change) || !change.valid() {
		return failed(400, "the body isn't an age")
	}
	user, found := service.users[id]
	if !found {
		return failed(404, fmt.Sprintf("no user %d", id))
	}
	user.age = *change.Age
	delete(service.cache, id)
	return Response{200, user.dump()}
}

func (service *Service) placeOrder(request *Request) Response {
	if !checkedBody(request) {
		return failed(415, "a JSON body is needed")
	}
	var made NewOrder
	if !decoded(request.body, &made) || !made.valid() {
		return failed(400, "the body isn't an order")
	}
	user, found := service.users[*made.User]
	if !found {
		return failed(404, fmt.Sprintf("no user %d", *made.User))
	}
	order := Order{id: service.nextOrder, item: *made.Item, quantity: *made.Quantity, price: *made.Price}
	service.nextOrder++
	user.spent += order.quantity * order.price
	user.orders++
	delete(service.cache, user.id)
	service.orders[user.id] = append(service.orders[user.id], order)
	return Response{201, string(order.appendTo(make([]byte, 0, 80)))}
}

// A user's latest orders, newest last, with how many there are and what they cost.
func (service *Service) listOrders(id int64) Response {
	if _, found := service.users[id]; !found {
		return failed(404, fmt.Sprintf("no user %d", id))
	}
	orders := service.orders[id]
	var total int64
	for _, order := range orders {
		total += order.quantity * order.price
	}
	text := fmt.Appendf(nil, `{"user": %d, "count": %d, "total": %d, "orders": [`, id, len(orders), total)
	first := max(0, len(orders)-shown)
	for at := first; at < len(orders); at++ {
		if at > first {
			text = append(text, ", "...)
		}
		text = orders[at].appendTo(text)
	}
	text = append(text, "]}"...)
	return Response{200, string(text)}
}

func (service *Service) route(request *Request) Response {
	path := request.path
	switch path {
	case "/users":
		if request.method != "POST" {
			return failed(405, "use POST")
		}
		return service.createUser(request)
	case "/orders":
		if request.method != "POST" {
			return failed(405, "use POST")
		}
		return service.placeOrder(request)
	case "/stats":
		return Response{200, service.stats}
	}
	if rest, found := strings.CutPrefix(path, "/users/"); found {
		id, err := strconv.ParseInt(rest, 10, 64)
		if err != nil {
			return failed(400, "a user is a number")
		}
		switch request.method {
		case "GET":
			return service.showUser(id)
		case "PATCH":
			return service.changeAge(id, request)
		}
		return failed(405, "use GET or PATCH")
	}
	if rest, found := strings.CutPrefix(path, "/orders/"); found {
		id, err := strconv.ParseInt(rest, 10, 64)
		if err != nil {
			return failed(400, "a user is a number")
		}
		return service.listOrders(id)
	}
	return failed(404, "nothing at "+path)
}

func reason(status int64) string {
	switch status {
	case 200:
		return "OK"
	case 201:
		return "Created"
	case 400:
		return "Bad Request"
	case 404:
		return "Not Found"
	case 405:
		return "Method Not Allowed"
	}
	return "Unsupported Media Type"
}

// Handles one request's text, giving the response's status and text.
func (service *Service) handle(text string) (int64, []byte) {
	response := failed(400, "not a request")
	if request, ok := parsed(text); ok {
		response = service.route(&request)
		section := strings.Split(request.path, "/")[1]
		service.hits[request.method+" "+section]++
	}
	out := make([]byte, 0, 96+len(response.body))
	out = fmt.Appendf(out, "HTTP/1.1 %d %s\r\n", response.status, reason(response.status))
	out = append(out, "Content-Type: application/json\r\n"...)
	out = fmt.Appendf(out, "Content-Length: %d\r\n\r\n", len(response.body))
	out = append(out, response.body...)
	return response.status, out
}

// The end of a day: what the users spent, who orders most, and how often each route was asked
// for, which then starts again from nothing.
func (service *Service) endDay(day int64) {
	var spent int64
	for _, user := range service.users {
		spent += user.spent
	}
	placed, busy := 0, 0
	for _, orders := range service.orders {
		placed += len(orders)
		if len(orders) >= 10 {
			busy++
		}
	}
	var text strings.Builder
	fmt.Fprintf(&text, `{"day": %d, "users": %d, "spent": %d, `, day, len(service.users), spent)
	fmt.Fprintf(&text, `"orders": %d, "busy": %d, "hits": {`, placed, busy)
	for at, name := range slices.Sorted(maps.Keys(service.hits)) {
		if at > 0 {
			text.WriteString(", ")
		}
		fmt.Fprintf(&text, `"%s": %d`, name, service.hits[name])
	}
	text.WriteString("}}")
	service.stats = text.String()
	clear(service.hits)
	clear(service.cache)
}

func main() {
	started := time.Now()
	texts := corpus(7)
	service := Service{
		users:  map[int64]*User{},
		orders: map[int64][]Order{},
		cache:  map[int64]string{},
		hits:   map[string]int64{},
		stats:  "{}",
	}
	var checksum, statuses int64
	for at := range int64(requests) {
		status, out := service.handle(texts[at%corpusSize])
		checksum = (checksum*31 + int64(len(out))*7 + status) % 1_000_000_007
		statuses += status
		if (at+1)%dayLength == 0 {
			service.endDay((at + 1) / dayLength)
		}
	}
	fmt.Println(checksum, statuses, len(service.users), len(service.stats))
	fmt.Println(service.stats)
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
