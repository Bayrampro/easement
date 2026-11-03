package db

type Client struct{}

func Scoped(id int64) *Client { return &Client{} }

func Tx() {}

func Raw() {}
