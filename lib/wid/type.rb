# typed: true

module Wid
  class Type
    String = Class.new(Type)
    Int = Class.new(Type)
    UInt = Class.new(Type)
    Float = Class.new(Type)
    Double = Class.new(Type)
    Bool = Class.new(Type)

    def ==(other)
      self.class == other.class
    end
  end
end

Dir[File.join(File.dirname(__FILE__), "types/*.rb")].each do |file|
  require_relative file
end
