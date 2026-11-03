# typed: true
class Document
  def self.host(uid)
    uid.to_s
  end

  def self.fetch(uid)
    Http.get(uid)
  end

  def self.both(uid)
    Http.get(uid)
  end
end
