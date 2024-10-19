# frozen_string_literal: true
# typed: strict

module Wid
  class Type < T::Enum
    extend T::Sig

    enums do
      String = new
      Int = new
      UInt = new
      Float = new
      Double = new
      Bool = new
    end

    sig { params(other: Type).returns(T::Boolean) }
    def ==(other)
      self.class == other.class
    end
  end
end

Dir[File.join(File.dirname(__FILE__), "types/*.rb")].each do |file|
  require_relative file
end
