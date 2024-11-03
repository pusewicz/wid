# frozen_string_literal: true

module Wid
  class Type
    def ==(other)
      self.class == other.class
    end
  end
end

Dir[File.join(File.dirname(__FILE__), "types/*.rb")].each do |file|
  require_relative file
end
