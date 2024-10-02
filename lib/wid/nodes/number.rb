module Wid
  module Nodes
    class Number < Expression
      def initialize(value)
        super(
          if value.include?(".")
            value.to_f
          else
            value.to_i
          end
        )
      end

      def ==(other) = value == other&.value

      def children = []
    end
  end
end
