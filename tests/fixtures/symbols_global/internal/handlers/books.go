package handlers

import "example.com/app/internal/db"

func List(c *db.Client) {
	db.Raw()
}
