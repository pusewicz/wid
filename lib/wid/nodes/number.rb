module Wid
  module Nodes
    class Number < Expression
      def initialize(value)
        if value.include?('.')
          @value = value.to_f
        else
          @value = value.to_i
        end
      end
      def ==(other) = value == other&.value
      def children = []
    end
  end
end
