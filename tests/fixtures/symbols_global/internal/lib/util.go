package lib

import "example.com/app/internal/db"

func Open() *db.Client { // easement:disable-line symbol -- legacy, remove with #57
	var c *db.Client
	return c
}

func Raw() {
	db.Raw()
}
